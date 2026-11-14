//! RigDeck core: radio-neutral control of amateur transceivers (Icom CI-V and Yaesu CAT),
//! with no UI. Written fresh from the radio makers' documentation (see `catalogue/`).

pub mod bcd;
pub mod cat;
pub mod catalogue;
pub mod civ;
pub mod discovery;
pub mod engine;
pub mod memory;
pub mod protocol;
pub mod rig;
pub mod rigctld;
pub mod scope;
pub mod sim;
pub mod tci;
pub mod transport;

pub use engine::{Engine, EngineConfig, EngineError};
pub use protocol::{Protocol, driver};
pub use rig::{Capabilities, Event, Mode, ModeSetting, Request, RigKind};
