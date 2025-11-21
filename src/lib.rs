#![recursion_limit = "512"]

#[cfg(feature = "dashboard")]
pub mod api;
pub mod config;
pub mod wireguard;

#[cfg(feature = "cloudflare")]
pub mod cloudflare;

#[cfg(feature = "aws")]
pub mod aws;
