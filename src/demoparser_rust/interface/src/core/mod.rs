//! Core module for the interface crate
//!
//! This module contains core functionality for the interface crate.

pub mod demo_processor;
pub mod game_event;
pub mod tick_processor;

// Re-export for backwards compatibility
pub use demo_processor::{DemoInfo, DemoProcessor, PlayerInfo, RoundInfo};
