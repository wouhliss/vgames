---
audience: internal
component: launcher
type: added
---
GAME-08: the Vulkan layer test now covers a swapchain resize (recreated with `oldSwapchain`) and a simulated device loss (feature `fault-injection`, tests only): the lost device presents untouched and a recreated device gets the overlay again.
