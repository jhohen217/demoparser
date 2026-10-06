//! Weapon mapping utility
//!
//! This module provides utility functions for mapping weapon names to their corresponding IDs.

/// Map weapon names to their corresponding numeric IDs
///
/// This function converts weapon names (like "glock", "ak47") to their numeric IDs
/// as used in the PlayerWeaponID field in the CSV output.
pub fn map_weapon_name_to_id(weapon_name: &str) -> String {
    match weapon_name {
        "glock" => "4",
        "ak47" => "7",
        "awp" => "9",
        "m4a1" => "16", // M4A1 without silencer
        "m4a4" => "16", // M4A4 (same ID as M4A1)
        "m4a1_silencer" => "60",
        "usp_silencer" => "61",
        "mp9" => "34",
        "p250" => "36", // P250 pistol
        "hegrenade" => "44",
        // Add more mappings as needed based on CS:GO weapon IDs
        "deagle" => "1",
        "elite" => "2",
        "fiveseven" => "3",
        "xm1014" => "25",
        "mac10" => "17",
        "ump45" => "24",
        "p90" => "19",
        "galilar" => "13", // Galil AR
        "galil" => "13",
        "famas" => "10",
        "ssg08" => "40", // Scout
        "aug" => "8",
        "sg556" => "39",
        "scar20" => "38",
        "g3sg1" => "11",
        "nova" => "35",
        "sawedoff" => "29",
        "mag7" => "27",
        "m249" => "14",
        "negev" => "28",
        "bizon" => "26",
        "taser" => "31",
        "hkp2000" => "32", // P2000
        "mp7" => "33",
        "mp5sd" => "23",
        "incgrenade" => "48",
        "molotov" => "46",
        "inferno" => "48",
        "tec9" => "30",
        "cz75a" => "63",
        "revolver" => "64",
        "knife" => "42",
        "knife_t" => "59",
        "flashbang" => "43",
        "smokegrenade" => "45",
        "decoy" => "47",
        _ => "0", // Default if weapon not recognized
    }
    .to_string()
}
