%{!?noire_version:%global noire_version 1.1.12}
%{!?noire_release:%global noire_release 1}
%{!?noire_daemon_stage:%global noire_daemon_stage /nonexistent/noire-daemon}
%{!?noire_ui_stage:%global noire_ui_stage /nonexistent/noire-ui}

Name:           noire
Version:        %{noire_version}
Release:        %{noire_release}%{?dist}
Summary:        Native PipeWire microphone noise suppression
License:        GPL-3.0-or-later AND CC-BY-SA-4.0
URL:            https://github.com/rayan6ms/noire
ExclusiveArch:  x86_64
Requires:       noire-daemon%{?_isa} = %{version}-%{release}
Requires:       noire-ui%{?_isa} = %{version}-%{release}

%description
Convenience package for the Noire per-user daemon, command-line client, and
GPUI desktop interface.

%package daemon
Summary:        Per-user Noire daemon and command-line client
License:        GPL-3.0-or-later
Requires:       pipewire-libs%{?_isa}

%description daemon
Native PipeWire microphone noise-suppression daemon and command-line client.

%package ui
Summary:        GPUI desktop interface for Noire
License:        GPL-3.0-or-later AND CC-BY-SA-4.0
Requires:       noire-daemon%{?_isa} = %{version}-%{release}

%description ui
Native GPUI interface for controlling the Noire per-user daemon.

%prep

%build

%check
test -x %{noire_daemon_stage}/usr/bin/noired
test -x %{noire_daemon_stage}/usr/bin/noirectl
test -x %{noire_ui_stage}/usr/bin/noire

%install
mkdir -p %{buildroot}
cp -a %{noire_daemon_stage}/. %{buildroot}/
cp -a %{noire_ui_stage}/. %{buildroot}/

%files

%files daemon
%license /usr/share/licenses/noire-daemon/LICENSE
/usr/bin/noired
/usr/bin/noirectl
/usr/lib/systemd/user/noire.service
/usr/share/dbus-1/services/io.github.rayan6ms.Noire.Noire1.service
/usr/share/dbus-1/interfaces/io.github.rayan6ms.Noire.Noire1.xml
/usr/share/doc/noire-daemon/config-v1.toml
/usr/share/man/man1/noired.1*
/usr/share/man/man1/noirectl.1*
/usr/share/bash-completion/completions/noirectl
/usr/share/zsh/site-functions/_noirectl
/usr/share/fish/vendor_completions.d/noirectl.fish

%files ui
%license /usr/share/licenses/noire-ui/LICENSE
%license /usr/share/licenses/noire-ui/icon-LICENSE
/usr/bin/noire
/usr/share/applications/io.github.rayan6ms.Noire.desktop
/usr/share/metainfo/io.github.rayan6ms.Noire.metainfo.xml
/usr/share/icons/hicolor/scalable/apps/io.github.rayan6ms.Noire.svg
/usr/share/man/man1/noire.1*

%changelog
* Mon Aug 31 2026 rayan6ms - 1.1.12-1
- Harden release validation, tray recovery feedback, and window styling

* Sat Aug 29 2026 rayan6ms - 1.1.11-1
- Fix startup tray warnings and synchronize window state with the tray

* Thu Aug 27 2026 rayan6ms - 1.1.10-1
- Fix portable window icons, controller spacing, tray lifetime, and meter telemetry

* Wed Aug 26 2026 rayan6ms - 1.1.9-1
- Remove mapped tray keepalive windows and align settings spacing

* Wed Aug 26 2026 rayan6ms - 1.1.8-1
- Fix tray-only startup, duplicate ghost windows, and AppImage login startup

* Tue Aug 25 2026 rayan6ms - 1.1.7-1
- Stabilize tray-host recovery, error dismissal, and degraded-state feedback

* Mon Aug 24 2026 rayan6ms - 1.1.6-1
- Harden device recovery, error feedback, and tray lifecycle handling

* Mon Aug 24 2026 rayan6ms - 1.1.5-1
- Improve live recovery, idle responsiveness, and degraded-state feedback

* Mon Aug 24 2026 rayan6ms - 1.1.4-1
- Fix desktop startup, AppImage graphics discovery, launcher activation, and idle meter cleanup

* Mon Aug 24 2026 rayan6ms - 1.1.3-1
- Synchronize tray controls, safe startup, and portable daemon lifetime

* Mon Aug 24 2026 rayan6ms - 1.1.2-1
- Fix legacy AppImage launchers, control layout, and signal-path icons

* Mon Aug 24 2026 rayan6ms - 1.1.1-1
- Fix portable startup, desktop behavior, and microphone labels

* Fri Aug 21 2026 rayan6ms - 1.1.0-1
- Replace GTK4 interface with GPUI and ship FastEnhancer-B 48 kHz

* Thu Aug 13 2026 rayan6ms - 1.0.0-1
- Initial stable release candidate

* Thu Aug 13 2026 rayan6ms - 0.1.0-1
- Initial development packaging
