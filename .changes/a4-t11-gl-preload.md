---
audience: internal
component: launcher
type: added
---
A4-T11: Linux OpenGL overlay hooks in the same library as the Vulkan layer (`LD_PRELOAD`: `glXSwapBuffers`,
`eglSwapBuffers`, the GetProcAddress functions and `dlsym`, so games that open libGL themselves are covered). The
cards are copied into the default framebuffer with `glBlitFramebuffer`; state is saved and restored, the GL error
queue is untouched, and the hooks stand down when the Vulkan layer is active. Tested in a game process on Xvfb.
