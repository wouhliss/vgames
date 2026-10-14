# Linux launch links

The launcher refreshes its per-user `vgames://` handler at startup. It uses the current AppImage path,
so moving an AppImage or replacing it during an update repairs the next launch's links. The handler lives
in `$XDG_DATA_HOME/applications/<bundle-id>-handler.desktop`, or the desktop's default data directory.

The deb and rpm declare `desktop-file-utils` and `xdg-utils`. For an AppImage, install these two packages
with your distribution's package manager before starting the launcher. They provide
`update-desktop-database` and `xdg-mime`.

Links continue through the launcher's installed-package, signature and launch checks. Registration accepts
the launcher path as data, including spaces and desktop-entry reserved characters; it does not add game arguments.
