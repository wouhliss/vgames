//! Cloud-save launcher client (PLAY-06). HTTP, file staging and launch hooks
//! are separate so authenticated requests never expose credentials to the UI.
pub mod blobs;
pub mod hook;
pub mod http;
pub mod locations;
pub mod policy;
pub mod restore;
pub mod scan;
pub mod sync;
pub mod transaction;
