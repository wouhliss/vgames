//! # vgames-overlay
//!
//! In-game overlay (docs/architecture/05-social.md §6). Owner: Agent 4.
//!
//! - `protocol`: length-prefixed frames exchanged over loopback TCP between the launcher's
//!   overlay broker and the in-game renderer (view models in, user actions out). Always built.
//! - `renderer` (feature): the code that runs **inside the game process**:
//!   - Windows: DLL injected at launch; hudhook hooks D3D9/11/12 and OpenGL `Present`/`SwapBuffers`.
//!   - Windows + Linux: Vulkan implicit layer (`vkQueuePresentKHR`), which also covers every Proton game.
//!   - Linux: `LD_PRELOAD` hooks for `glXSwapBuffers` / `eglSwapBuffers`.
//!   - Dear ImGui toasts and panel.
//!
//! Rules for the in-game side: no network except the broker link, no disk writes, no keys,
//! no allocation on the per-frame fast path while hidden, and never crash the game (every hook
//! catches panics and disables itself).
