# Controller mapping database

`gamecontrollerdb.txt` and `gamecontrollerdb.LICENSE` are unchanged copies from
[SDL_GameControllerDB](https://github.com/mdqinc/SDL_GameControllerDB) at commit
`c1d5289a1f713b30a2c121e9fe6529d39360be0f`. The database uses SDL's compatible
2.0.16 mapping format and is distributed under the accompanying zlib license.

PLAY updates the pinned database and license together for launcher releases.
The input service loads the bundled copy; it never downloads mappings at runtime.
