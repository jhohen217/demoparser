use interface::models::collection::KillCollection;
use std::path::Path;

/// Struct to hold all formatted array strings for a collection
pub struct CollectionArrays {
    pub weapons_str: String,
    pub weapons_id_str: String,
    pub kill_ticks_str: String,
    pub victims_index_str: String,
    pub victim_names: String,
    pub weapons_formatted: String,
    pub killer_pos_x: String,
    pub killer_pos_y: String,
    pub killer_pos_z: String,
    pub killer_pitch: String,
    pub killer_yaw: String,
    pub victim_pos_x: String,
    pub victim_pos_y: String,
    pub victim_pos_z: String,
    pub victim_dist: String,
    pub ticks_between: String,
    pub move_between: String,
    pub kill_weapon_ids: String,
}

/// Prepare collection data (drive letter, relative path, and arrays)
pub fn prepare_collection_data(
    collection: &KillCollection,
    base_path_str: &str,
) -> (String, String, CollectionArrays) {
    // Extract drive letter
    let drive_letter = if cfg!(windows) {
        let path_str = &collection.demo_path;
        if path_str.len() >= 2 && path_str.chars().nth(1) == Some(':') {
            path_str[..2].to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    // Calculate relative path
    let full_path_str = collection.demo_path.replace('\\', "/");
    let relative_path = if !base_path_str.is_empty() && full_path_str.starts_with(base_path_str) {
        full_path_str[base_path_str.len()..].to_string()
    } else {
        if let Some(parent) = Path::new(&collection.demo_path).parent() {
            parent
                .file_name()
                .map(|s| s.to_string_lossy().to_string() + "/")
                .unwrap_or_default()
        } else {
            String::new()
        }
    };

    // Format arrays
    let arrays = format_collection_arrays(collection);

    (drive_letter, relative_path, arrays)
}

/// Format all array fields for a collection
fn format_collection_arrays(collection: &KillCollection) -> CollectionArrays {
    let weapons_str = collection.weapons.join(";");
    let weapons_id_str = collection.weapons_id.join(";");
    let kill_ticks_str = collection
        .kill_ticks
        .iter()
        .map(|t| t.to_string())
        .collect::<Vec<_>>()
        .join(";");
    let victims_index_str = collection
        .victim_indices
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(";");

    // Format weapons damaged hits
    let mut weapons_formatted = String::new();
    if collection.weapons_damaged.len() > 2 && collection.weapons_damaged_num_hits.len() > 2 {
        let w_str = &collection.weapons_damaged[1..collection.weapons_damaged.len() - 1];
        let h_str =
            &collection.weapons_damaged_num_hits[1..collection.weapons_damaged_num_hits.len() - 1];

        if !w_str.is_empty() {
            let parts: Vec<&str> = w_str.split(';').collect();
            let hits: Vec<&str> = h_str.split(';').collect();

            if parts.len() == hits.len() {
                let formatted_parts: Vec<String> = parts
                    .iter()
                    .zip(hits.iter())
                    .map(|(w, h)| format!("{}({})", w, h))
                    .collect();
                weapons_formatted = format!("[{}]", formatted_parts.join(" - "));
            }
        }
    }

    // Build position arrays
    let mut sorted_kills = collection.kills.clone();
    sorted_kills.sort_by_key(|k| k.tick);

    let mut killer_pos_x = Vec::new();
    let mut killer_pos_y = Vec::new();
    let mut killer_pos_z = Vec::new();
    let mut killer_pitch = Vec::new();
    let mut killer_yaw = Vec::new();
    let mut victim_names = Vec::new();
    let mut victim_pos_x = Vec::new();
    let mut victim_pos_y = Vec::new();
    let mut victim_pos_z = Vec::new();
    let mut victim_dist = Vec::new();
    let mut ticks_between = Vec::new();
    let mut move_between = Vec::new();
    let mut kill_weapon_ids = Vec::new();

    let mut prev_kill: Option<interface::models::kill::Kill> = None;

    for kill in &sorted_kills {
        victim_names.push(kill.victim_name.clone());

        killer_pos_x.push(format!("{:.6}", kill.killer_pos_x));
        killer_pos_y.push(format!("{:.6}", kill.killer_pos_y));
        killer_pos_z.push(format!("{:.6}", kill.killer_pos_z));
        killer_pitch.push(format!("{:.6}", kill.killer_view_pitch));
        killer_yaw.push(format!("{:.6}", kill.killer_view_yaw));

        victim_pos_x.push(format!("{:.6}", kill.victim_pos_x));
        victim_pos_y.push(format!("{:.6}", kill.victim_pos_y));
        victim_pos_z.push(format!("{:.6}", kill.victim_pos_z));

        victim_dist.push(format!("{:.6}", kill.distance_to_enemy));
        kill_weapon_ids.push(kill.weapon_id.clone());

        if let Some(prev) = &prev_kill {
            ticks_between.push((kill.tick - prev.tick).to_string());
            let dist = interface::utils::calculations::calculate_distance(
                &[prev.killer_pos_x, prev.killer_pos_y, prev.killer_pos_z],
                &[kill.killer_pos_x, kill.killer_pos_y, kill.killer_pos_z],
            );
            move_between.push(format!("{:.6}", dist));
        } else {
            ticks_between.push("0".to_string());
            move_between.push("0.000000".to_string());
        }
        prev_kill = Some(kill.clone());
    }

    CollectionArrays {
        weapons_str,
        weapons_id_str,
        kill_ticks_str,
        victims_index_str,
        victim_names: victim_names.join(";"),
        weapons_formatted,
        killer_pos_x: killer_pos_x.join(";"),
        killer_pos_y: killer_pos_y.join(";"),
        killer_pos_z: killer_pos_z.join(";"),
        killer_pitch: killer_pitch.join(";"),
        killer_yaw: killer_yaw.join(";"),
        victim_pos_x: victim_pos_x.join(";"),
        victim_pos_y: victim_pos_y.join(";"),
        victim_pos_z: victim_pos_z.join(";"),
        victim_dist: victim_dist.join(";"),
        ticks_between: ticks_between.join(";"),
        move_between: move_between.join(";"),
        kill_weapon_ids: kill_weapon_ids.join(";"),
    }
}
