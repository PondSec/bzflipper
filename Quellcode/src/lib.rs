//! PursePilot (Bazaar Auction Flipper) for Hypixel Skyblock
//!
//! A high-performance Minecraft bot for automated bazaar and auction house flipping.
//! Rust port of the original TypeScript implementation using the Azalea framework.

pub mod bazaar_scanner;
pub mod bazaar_tracker;
pub mod bot;
pub mod config;
pub mod gui;
pub mod handlers;
pub mod inventory;
pub mod logging;
pub mod profit;
pub mod state;
pub mod types;
pub mod utils;
pub mod vps;
pub mod web;
pub mod webhook;
pub mod websocket;

pub use bot::{BotClient, BotEvent, BotEventHandlers};
pub use types::{BazaarFlipRecommendation, BotState, CommandPriority, CommandType, Flip};
pub use web::{start_web_server, WebSharedState};
