# In-game overlay renderers

CI stages the renderers here before bundling (`bundle.resources` maps this directory to
`overlay/` in Tauri's resource directory); they are build outputs and never committed:

| OS | Files |
|---|---|
| Linux | `libvgames_overlay.so` (Vulkan implicit layer and GL preload) |
| Windows | `vgames_overlay64.dll`, `vgames_overlay32.dll`, `vgames-inject32.exe` (when built) |

The launcher looks for them under `overlay/` in its resource directory
(`overlay::vulkan_layer::installed_library`) and runs without them.
