//! Brings MowBite OS onto a mower and back: the frame and the interface of the mower modules.

pub mod awake;
pub mod cli;
pub mod disk;
pub mod fetch;
pub mod mower;
pub mod net;
pub mod release;
pub mod run;
pub mod secret;
pub mod sign;
pub mod space;
pub mod store;
#[cfg(test)]
mod testing;
pub mod text;
pub mod tron;
