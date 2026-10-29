//! SigmaOS Mintupdate
//! Mint update manager

pub mod mintupdate;

pub use mintupdate::{
    AutomationPolicy, MintUpdateManager, MintUpdateManager as Mintupdate, Update, UpdateState,
    UpdateType,
};
