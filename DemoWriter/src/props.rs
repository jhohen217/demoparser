//! Sample player properties at a tick, so a clip can be compared against its source.
//!
//! This is the fidelity check that matters: identical round boundaries only prove the
//! event stream survived, not that entity state did.

use ahash::AHashMap;
use anyhow::{anyhow, Result};
use parser::first_pass::parser_settings::{rm_user_friendly_names, ParserInputs};
use parser::parse_demo::{Parser, ParsingMode};
use parser::second_pass::parser_settings::create_huffman_lookup_table;
use parser::second_pass::variants::{PropColumn, VarVec};

pub const DEFAULT_PROPS: [&str; 6] = ["balance", "health", "armor_value", "team_num", "X", "Y"];

/// One row per player slot, in the parser's own column order.
pub fn sample(
    demo: &[u8],
    tick: i32,
    props: &[String],
    single_threaded: bool,
) -> Result<Vec<(String, Vec<String>)>> {
    let real_props = rm_user_friendly_names(&props.to_vec())
        .map_err(|e| anyhow!("could not resolve properties: {e}"))?;
    let mut real_name_to_og_name = AHashMap::default();
    for (real, friendly) in real_props.iter().zip(props) {
        real_name_to_og_name.insert(real.clone(), friendly.clone());
    }

    let huffman_lookup_table = create_huffman_lookup_table().to_vec();
    let settings = ParserInputs {
        real_name_to_og_name,
        wanted_players: vec![],
        wanted_player_props: real_props.clone(),
        wanted_other_props: vec![],
        wanted_prop_states: AHashMap::default(),
        wanted_ticks: vec![tick],
        wanted_events: vec![],
        parse_ents: true,
        parse_projectiles: false,
        parse_grenades: false,
        only_header: false,
        only_convars: false,
        huffman_lookup_table: &huffman_lookup_table,
        order_by_steamid: false,
        list_props: false,
        fallback_bytes: None,
    };

    let mut parser = Parser::new(
        settings,
        if single_threaded {
            ParsingMode::ForceSingleThreaded
        } else {
            ParsingMode::ForceMultiThreaded
        },
    );
    let output = parser
        .parse_demo(demo)
        .map_err(|e| anyhow!("could not parse demo: {e}"))?;

    // Map the parser's numeric prop ids back to the names we asked for.
    let mut by_name: Vec<(String, Vec<String>)> = Vec::new();
    let mut ids: Vec<(u32, String)> = output
        .prop_controller
        .prop_infos
        .iter()
        .map(|info| (info.id, info.prop_friendly_name.clone()))
        .collect();
    ids.sort_by_key(|(id, _)| *id);
    ids.dedup_by_key(|(id, _)| *id);

    for (id, name) in ids {
        let Some(column) = output.df.get(&id) else {
            continue;
        };
        if !props.iter().any(|p| *p == name)
            && name != "steamid"
            && name != "name"
            && name != "tick"
        {
            continue;
        }
        by_name.push((name, column_to_strings(column)));
    }
    Ok(by_name)
}

fn column_to_strings(column: &PropColumn) -> Vec<String> {
    let Some(data) = &column.data else {
        return vec![];
    };
    match data {
        VarVec::I32(v) => v.iter().map(opt_to_string).collect(),
        VarVec::U32(v) => v.iter().map(opt_to_string).collect(),
        VarVec::U64(v) => v.iter().map(opt_to_string).collect(),
        VarVec::F32(v) => v
            .iter()
            .map(|x| match x {
                Some(value) => format!("{value:.1}"),
                None => "-".to_string(),
            })
            .collect(),
        VarVec::Bool(v) => v.iter().map(opt_to_string).collect(),
        VarVec::String(v) => v
            .iter()
            .map(|x| x.clone().unwrap_or_else(|| "-".to_string()))
            .collect(),
        VarVec::StringVec(v) => v.iter().map(|x| format!("{x:?}")).collect(),
        VarVec::Binary(v) => v.iter().map(|x| format!("{x:?}")).collect(),
        VarVec::U64Vec(v) => v.iter().map(|x| format!("{x:?}")).collect(),
        VarVec::XYVec(v) => v.iter().map(|x| format!("{x:?}")).collect(),
        VarVec::XYZVec(v) => v.iter().map(|x| format!("{x:?}")).collect(),
        VarVec::U32Vec(v) => v.iter().map(|x| format!("{x:?}")).collect(),
        VarVec::Stickers(v) => v.iter().map(|x| format!("{x:?}")).collect(),
        VarVec::InputHistory(v) => v.iter().map(|x| format!("{x:?}")).collect(),
        VarVec::UserCmdSubtickMoves(v) => v.iter().map(|x| format!("{x:?}")).collect(),
    }
}

fn opt_to_string<T: std::fmt::Display>(value: &Option<T>) -> String {
    match value {
        Some(v) => v.to_string(),
        None => "-".to_string(),
    }
}
