# Noire translations

Noire uses the gettext domain `noire` through GLib, which is already part of the
GTK runtime. No additional application runtime dependency is required.

Regenerate the source template after changing calls to `tr(...)`:

```sh
po/update-pot.sh
```

Add a locale code to `po/LINGUAS`, translate `po/noire.pot` into
`po/<locale>.po`, and compile it to
`po/locale/<locale>/LC_MESSAGES/noire.mo`. The native payload staging script
installs compiled catalogs beneath `/usr/share/locale`. Do not add a locale to
`LINGUAS` until its catalog has been reviewed in the actual GTK interface.
