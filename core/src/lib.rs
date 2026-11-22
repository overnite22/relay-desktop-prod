//! Data layer of the native Relay desktop client: a shared model, the
//! [`DataSource`] trait and its two implementations (local encrypted
//! database, paired Relay server).

pub mod admin;
pub mod backup;
pub mod cast;
pub mod command;
pub mod conn;
pub mod local;
pub mod markdown;
pub mod model;
pub mod moves;
pub mod pairing;
pub mod probe;
pub mod rdmimport;
pub mod server;
pub mod source;
pub mod ssh;
pub mod termsel;
pub mod totp;
pub mod update;
pub mod vault;

pub use local::LocalSource;
pub use model::*;
pub use server::ServerSource;
pub use source::{effective_credential, DataSource, Error, Result};
