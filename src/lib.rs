// TODO: Remove this later
// #![allow(warnings)]

#[macro_use]
extern crate log;
#[macro_use]
extern crate derive_builder;

mod behaviours;
mod commands;
mod config;
mod constants;
mod errors;
mod events;
mod message;
mod node;
mod utils;

pub use commands::Command;
pub use config::{Config, ConfigBuilder};
pub use errors::Error;
pub use events::Event;
pub use message::Message;
pub use node::Node;

// Re-export tokio
pub use tokio;

pub mod prelude {
  pub type Ret<T = (), E = anyhow::Error> = anyhow::Result<T, E>;
  pub use std::format as f;

  pub struct Wrap<T>(pub T);
}
