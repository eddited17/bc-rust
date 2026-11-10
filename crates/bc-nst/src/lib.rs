//! bc-nst: the client services endpoint of a Business Central service tier,
//! implemented on top of bc-rust's AL runtime.
//!
//! Module ownership (parallel work, one owner per module):
//! - `wire`:     shared JSON wire helpers ($polyType, typeHint wrappers, ServerFault)
//! - `rpc`:      JSON-RPC 2.0 over WebSocket at /{instance}/client/ws/connect, sessions, IClientApi methods
//! - `metadata`: REST /{instance}/client/metadata/* (MasterPage / MetaTable generation from al-model)
//! - `dataset`:  NavDataSet binary encoding for GetPage results, bookmarks

pub mod auth;
pub mod dataset;
pub mod form;
#[cfg(test)]
mod form_tests;
#[cfg(test)]
mod format_tests;
pub mod metadata;
pub mod pbt;
pub mod platform;
pub mod rpc;
pub mod track;
pub mod ui;
pub mod wire;
