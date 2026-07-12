//! Bottom-screen recording overlay.
//!
//! The overlay is optional and has native Wayland layer-shell and X11 backends.

#[cfg(feature = "overlay")]
mod service;
#[cfg(feature = "overlay")]
mod transcript;

#[cfg(feature = "overlay")]
pub use service::spawn_overlay;

#[cfg(not(feature = "overlay"))]
pub async fn spawn_overlay(
    _state_rx: tokio::sync::watch::Receiver<crate::State>,
    _level_rx: tokio::sync::watch::Receiver<f32>,
    _text_rx: tokio::sync::watch::Receiver<String>,
    _config: crate::OverlayConfig,
) {
    tracing::warn!("overlay feature is disabled at compile time");
}
