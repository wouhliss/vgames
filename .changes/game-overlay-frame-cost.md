---
audience: internal
component: launcher
type: added
---
GAME-08: the overlay tests assert the frame cost against 05-social §6.1 for EGL, GLX and Vulkan: hidden swap/present
overhead ≤ 0.3 ms over the same app without the preload (or an instance without the layer), with the panel open the
overlay's own draw time ≤ 1 ms per frame and the whole frame (driver raster work included) ≤ 1 ms slower, every
measured frame drawn, and opening the panel ≤ 100 ms in all. CI prints the figures in the job summary. The GL safety
valve now counts a frame that creates our GL objects (also after a context is recreated at the same address) minus a
250 ms warm-up allowance, instead of the full one-off driver warm-up that could turn the overlay off on llvmpipe.
