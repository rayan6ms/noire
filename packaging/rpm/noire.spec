%{!?noire_version:%global noire_version 1.0.0}
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
optional GTK4 interface.

%package daemon
Summary:        Per-user Noire daemon and command-line client
License:        GPL-3.0-or-later
Requires:       pipewire-libs%{?_isa}

%description daemon
Native PipeWire microphone noise-suppression daemon and command-line client.

%package ui
Summary:        GTK4 interface for Noire
License:        GPL-3.0-or-later AND CC-BY-SA-4.0
Requires:       noire-daemon%{?_isa} = %{version}-%{release}
Requires:       gtk4%{?_isa} >= 4.10
Requires:       libglvnd-gles%{?_isa}

%description ui
Optional GTK4 interface for controlling the Noire per-user daemon.

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
/usr/share/doc/noire-daemon/USER_GUIDE.md
/usr/share/doc/noire-daemon/TROUBLESHOOTING.md
/usr/share/doc/noire-daemon/PRIVACY.md
/usr/share/doc/noire-daemon/SECURITY.md
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
* Thu Aug 13 2026 rayan6ms - 1.0.0-1
- Initial stable release candidate

* Thu Aug 13 2026 rayan6ms - 0.1.0-1
- Initial development packaging
