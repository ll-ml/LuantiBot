//! Luanti protocol bot library.
//!
//! The binary is intentionally a thin wrapper around [`run`]. Protocol I/O,
//! game simulation, bot control, the HTTP boundary, and the LLM agent live in
//! separate modules so they can be tested without starting a live session.

mod agent;
mod api;
mod app;
mod bot;
mod codec;
mod game;
mod network;
mod types;
mod world;

pub use app::run;
