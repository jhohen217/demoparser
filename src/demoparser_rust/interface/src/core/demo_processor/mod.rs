//! Demo processor module
//!
//! This module provides functionality for processing CS:GO demo files.
//! It has been refactored into separate submodules for better organization.

pub mod death_parser;
pub mod game_parser;
pub mod parser;
pub mod player_parser;
pub mod round_parser;
mod team_timeline;
pub mod types;

// Re-export the main types and struct for convenience
pub use parser::DemoProcessor;
pub use types::{DemoInfo, PlayerInfo, RoundInfo};
