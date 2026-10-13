//! Cloud-save launcher client (PLAY-06). HTTP, file staging and launch hooks
//! are separate so authenticated requests never expose credentials to the UI.
pub mod http;
