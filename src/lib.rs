//! Brings MowBite OS onto a mower and back: the frame and the interface of the mower modules.

pub mod awake;
pub mod disk;
pub mod mower;
pub mod run;
pub mod secret;
pub mod sign;
pub mod store;
#[cfg(test)]
mod testing;
pub mod text;
