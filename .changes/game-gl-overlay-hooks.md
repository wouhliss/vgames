---
audience: internal
component: launcher
type: added
---
GAME-08: the overlay library hooks glXSwapBuffers/eglSwapBuffers when preloaded (exports plus a naked x86_64 dlsym trampoline for games that load GL by handle), blits the cards through an FBO with the game's GL state saved and restored, stands down once the Vulkan layer draws, and is tested on llvmpipe (EGL surfaceless and GLX under Xvfb) with `vgames-gl-testapp`. The launcher does not preload it yet.
