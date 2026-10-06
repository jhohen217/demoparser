//! Button parsing logic for CS2 demo files
//!
//! This module handles the extraction and interpretation of button presses
//! from CS2 demo data, including the button mask parsing and individual
//! button state extraction.

use ahash::AHashMap;
use parser::second_pass::variants::{PropColumn, VarVec};
use std::collections::HashMap;

/// The decoded state sampled for a row, without command-time or freshness authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonSource {
    MovementPrevious,
    UserCommandState1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonObservation {
    pub mask: u64,
    pub source: ButtonSource,
}

/// CS2 Button bit positions in the button mask
/// These correspond to the bit positions used in CS2's button encoding
/// Note: CS2 uses 64-bit button masks for some buttons like INSPECT
pub struct ButtonBits;

impl ButtonBits {
    pub const FIRE: u64 = 1; // Bit 0 (value 1)
    pub const FORWARD: u64 = 8; // Bit 3 (value 8)
    pub const BACK: u64 = 16; // Bit 4 (value 16)
    pub const LEFT: u64 = 512; // Bit 9 (value 512)
    pub const RIGHT: u64 = 1024; // Bit 10 (value 1024)
    pub const RIGHT_CLICK: u64 = 2048; // Bit 11 (value 2048)
    pub const JUMP: u64 = 2; // Bit 1 (value 2)
    pub const DUCK: u64 = 4; // Bit 2 (value 4)
    pub const RELOAD: u64 = 8192; // Bit 13 (value 8192)
    pub const USE: u64 = 32; // Bit 5 (value 32)
    pub const WALK: u64 = 256; // Bit 8 (value 256)
    pub const INSPECT: u64 = 34359738368; // Bit 35 (value 34359738368) - Corrected!
}

/// Individual button states extracted from the button mask
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct ButtonStates {
    pub forward: u32,
    pub back: u32,
    pub left: u32,
    pub right: u32,
    pub fire: u32,
    pub right_click: u32,
    pub jump: u32,
    pub duck: u32,
    pub reload: u32,
    pub use_key: u32,
    pub walk: u32,
    pub inspect: u32,
}

/// Button parser for extracting button data from demo output
pub struct ButtonParser;

impl ButtonParser {
    /// Get the list of button-related property names to request during parsing
    pub fn get_button_property_names() -> Vec<String> {
        vec![
            // Primary button mask source
            "CCSPlayerPawn.CCSPlayer_MovementServices.m_nButtonDownMaskPrev".to_string(),
            // Fallback button sources
            "CCSPlayer_MovementServices.m_nButtonDownMaskPrev".to_string(),
            "m_nButtonDownMaskPrev".to_string(),
            "usercmd_buttonstate_1".to_string(),
            "usercmd_buttonstate_2".to_string(),
            "usercmd_buttonstate_3".to_string(),
        ]
    }

    /// Extract the button mask from the dataframe for a given index
    pub fn find_button_mask(
        df: &AHashMap<u32, PropColumn>,
        name_to_id: &HashMap<String, u32>,
        index: usize,
        debug: bool,
    ) -> Option<ButtonObservation> {
        if debug && index == 0 {
            println!("DEBUG: Looking for button mask properties...");
        }

        // Try primary property first
        if let Some(button_id) = name_to_id.get("CCSPlayer_MovementServices.m_nButtonDownMaskPrev")
        {
            if debug && index == 0 {
                println!("DEBUG: Found CCSPlayer_MovementServices.m_nButtonDownMaskPrev property with ID {}", button_id);
            }

            if let Some(mask) = Self::extract_u64_from_column(df, button_id, index, debug) {
                return Some(ButtonObservation { mask, source: ButtonSource::MovementPrevious });
            }
        } else if debug && index == 0 {
            println!("DEBUG: CCSPlayer_MovementServices.m_nButtonDownMaskPrev property not found");
        }

        // Try fallback properties
        let fallback_properties = [
            "CCSPlayerPawn.CCSPlayer_MovementServices.m_nButtonDownMaskPrev",
            "m_nButtonDownMaskPrev",
            "usercmd_buttonstate_1",
        ];

        for prop_name in &fallback_properties {
            if let Some(button_id) = name_to_id.get(*prop_name) {
                if debug && index == 0 {
                    println!("DEBUG: Trying fallback property: {}", prop_name);
                }
                if let Some(mask) = Self::extract_u64_from_column(df, button_id, index, debug) {
                    return Some(ButtonObservation {
                        mask,
                        source: if *prop_name == "usercmd_buttonstate_1" {
                            ButtonSource::UserCommandState1
                        } else {
                            ButtonSource::MovementPrevious
                        },
                    });
                }
            }
        }

        if debug && index == 0 {
            println!("DEBUG: No usable button observation found");
        }

        None
    }

    /// Extract individual button states from a button mask
    pub fn extract_button_states(button_mask: u64) -> ButtonStates {
        ButtonStates {
            forward: if (button_mask & ButtonBits::FORWARD) != 0 {
                1
            } else {
                0
            },
            back: if (button_mask & ButtonBits::BACK) != 0 {
                1
            } else {
                0
            },
            left: if (button_mask & ButtonBits::LEFT) != 0 {
                1
            } else {
                0
            },
            right: if (button_mask & ButtonBits::RIGHT) != 0 {
                1
            } else {
                0
            },
            fire: if (button_mask & ButtonBits::FIRE) != 0 {
                1
            } else {
                0
            },
            right_click: if (button_mask & ButtonBits::RIGHT_CLICK) != 0 {
                1
            } else {
                0
            },
            jump: if (button_mask & ButtonBits::JUMP) != 0 {
                1
            } else {
                0
            },
            duck: if (button_mask & ButtonBits::DUCK) != 0 {
                1
            } else {
                0
            },
            reload: if (button_mask & ButtonBits::RELOAD) != 0 {
                1
            } else {
                0
            },
            use_key: if (button_mask & ButtonBits::USE) != 0 {
                1
            } else {
                0
            },
            walk: if (button_mask & ButtonBits::WALK) != 0 {
                1
            } else {
                0
            },
            inspect: if (button_mask & ButtonBits::INSPECT) != 0 {
                1
            } else {
                0
            },
        }
    }

    /// Debug print button states (debug builds only)
    #[cfg(debug_assertions)]
    pub fn debug_print_buttons(button_mask: u64, states: &ButtonStates) {
        println!("DEBUG: Button extraction from mask 0x{:016X}:", button_mask);
        println!("  FORWARD (bit 3): {}", states.forward);
        println!("  BACK (bit 4): {}", states.back);
        println!("  LEFT (bit 9): {}", states.left);
        println!("  RIGHT (bit 10): {}", states.right);
        println!("  FIRE (bit 0): {}", states.fire);
        println!("  RIGHT CLICK (bit 11): {}", states.right_click);
        println!("  JUMP (bit 1): {}", states.jump);
        println!("  DUCK (bit 2): {}", states.duck);
        println!("  RELOAD (bit 13): {}", states.reload);
        println!("  USE (bit 5): {}", states.use_key);
        println!("  WALK (bit 8): {}", states.walk);
        println!("  INSPECT (bit 35): {}", states.inspect);
    }

    /// Debug print button states (no-op in release builds)
    #[cfg(not(debug_assertions))]
    pub fn debug_print_buttons(_button_mask: u64, _states: &ButtonStates) {
        // No-op in release builds
    }

    /// Helper function to extract u64 value from any column type
    fn extract_u64_from_column(
        df: &AHashMap<u32, PropColumn>,
        prop_id: &u32,
        index: usize,
        debug: bool,
    ) -> Option<u64> {
        if let Some(column) = df.get(prop_id) {
            if debug && index == 0 {
                println!("DEBUG: Column found, checking data type...");
            }

            if let Some(data) = &column.data {
                match data {
                    VarVec::U64(values) => {
                        if debug && index == 0 {
                            println!("DEBUG: Data is U64 vector with {} elements", values.len());
                        }
                        if let Some(Some(value)) = values.get(index) {
                            if debug && index == 0 {
                                println!("DEBUG: Found U64 value: {}", value);
                            }
                            return Some(*value);
                        } else if debug && index == 0 {
                            println!("DEBUG: No value at index {} or value is None", index);
                        }
                    }
                    VarVec::U32(values) => {
                        if debug && index == 0 {
                            println!("DEBUG: Data is U32 vector with {} elements", values.len());
                        }
                        if let Some(Some(value)) = values.get(index) {
                            if debug && index == 0 {
                                println!("DEBUG: Found U32 value: {}", value);
                            }
                            return Some(*value as u64);
                        } else if debug && index == 0 {
                            println!("DEBUG: No value at index {} or value is None", index);
                        }
                    }
                    VarVec::I32(values) => {
                        if debug && index == 0 {
                            println!("DEBUG: Data is I32 vector with {} elements", values.len());
                        }
                        if let Some(Some(value)) = values.get(index) {
                            if debug && index == 0 {
                                println!("DEBUG: Found I32 value: {}", value);
                            }
                            return u64::try_from(*value).ok();
                        } else if debug && index == 0 {
                            println!("DEBUG: No value at index {} or value is None", index);
                        }
                    }
                    _ => {
                        if debug && index == 0 {
                            println!(
                                "DEBUG: Data is some other type: {:?}",
                                std::mem::discriminant(data)
                            );
                        }
                    }
                }
            } else if debug && index == 0 {
                println!("DEBUG: Column exists but data is None");
            }
        } else if debug && index == 0 {
            println!("DEBUG: Column not found in dataframe");
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(primary: Option<VarVec>, fallback: Option<VarVec>, index: usize) -> Option<ButtonObservation> {
        let df = AHashMap::from([
            (1, PropColumn { data: primary, num_nones: 0 }),
            (2, PropColumn { data: fallback, num_nones: 0 }),
        ]);
        let names = HashMap::from([("m_nButtonDownMaskPrev".into(), 1), ("usercmd_buttonstate_1".into(), 2)]);
        ButtonParser::find_button_mask(&df, &names, index, false)
    }

    #[test]
    fn input_observation_zero_wins_over_positive_fallback() {
        assert_eq!(find(Some(VarVec::U64(vec![Some(0)])), Some(VarVec::U64(vec![Some(1)])), 0),
            Some(ButtonObservation { mask: 0, source: ButtonSource::MovementPrevious }));
        assert_eq!(find(Some(VarVec::U64(vec![None])), Some(VarVec::U64(vec![Some(0)])), 0),
            Some(ButtonObservation { mask: 0, source: ButtonSource::UserCommandState1 }));
    }

    #[test]
    fn input_observation_missing_invalid_and_out_of_range_are_unknown() {
        assert_eq!(find(None, None, 0), None);
        assert_eq!(find(Some(VarVec::U64(vec![Some(1)])), None, 1), None);
        assert_eq!(find(Some(VarVec::I32(vec![Some(-1)])), None, 0), None);
        for value in [0.0, 1.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(find(Some(VarVec::F32(vec![Some(value)])), None, 0), None);
        }
        assert_eq!(ButtonParser::find_button_mask(&AHashMap::new(), &HashMap::new(), 0, false), None);
        assert_eq!(find(Some(VarVec::U64(vec![Some(1 << 35)])), None, 0).unwrap().mask, 1 << 35);
    }

    #[test]
    fn test_button_extraction() {
        // Test with a known button mask
        let button_mask = 8 | 512 | 1 | 2048; // FORWARD + LEFT + FIRE + RIGHT CLICK
        let states = ButtonParser::extract_button_states(button_mask);

        assert_eq!(states.forward, 1);
        assert_eq!(states.left, 1);
        assert_eq!(states.fire, 1);
        assert_eq!(states.right_click, 1);
        assert_eq!(states.back, 0);
        assert_eq!(states.right, 0);
    }

    #[test]
    fn test_empty_button_mask() {
        let button_mask = 0;
        let states = ButtonParser::extract_button_states(button_mask);

        assert_eq!(states.forward, 0);
        assert_eq!(states.back, 0);
        assert_eq!(states.left, 0);
        assert_eq!(states.right, 0);
        assert_eq!(states.fire, 0);
        assert_eq!(states.right_click, 0);
    }
}
