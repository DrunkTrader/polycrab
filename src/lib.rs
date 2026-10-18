//! PolyCrab — shared library.
//!
//! Polymarket copy-trading engine and terminal operator dashboard. The current
//! supported bot is [`bot::copy_trading`].

pub mod bot;
pub mod config;
pub mod models;
pub mod operator;
pub mod service;
pub mod ui;
pub mod utils;
