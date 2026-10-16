---
audience: internal
component: launcher
type: security
---
`vgames_core::manifest::is_denied_env_key` now refuses every `LD_*` key, `PYTHONHOME`, `GCONV_PATH`, `BASH_ENV`, `ENV` and the Vulkan loader's driver and layer keys in package, compat profile and local override env, as 02 §5 lists (GAME-14 threat review C2).
