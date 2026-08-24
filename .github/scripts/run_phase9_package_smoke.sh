#!/bin/sh
set -eu

repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
work_dir=$(mktemp -d)
trap 'rm -rf -- "$work_dir"' EXIT HUP INT TERM

binary_dir="$work_dir/bin"
stage_dir="$work_dir/stage"
artifact_dir="$work_dir/artifacts"
mkdir -p "$binary_dir" "$artifact_dir"

for binary in noire noirectl noired; do
    {
        echo '#!/bin/sh'
        echo "echo '$binary 1.1.0'"
    } >"$binary_dir/$binary"
    chmod 0755 "$binary_dir/$binary"
done

if sh "$repo_dir/packaging/validate-binaries.sh" \
    1.1.0 x86_64 "$binary_dir" >/dev/null 2>&1; then
    echo "Release validation unexpectedly accepted non-ELF test binaries" >&2
    exit 1
fi
NOIRE_PACKAGE_ALLOW_TEST_BINARIES=1 \
    sh "$repo_dir/packaging/validate-binaries.sh" 1.1.0 x86_64 "$binary_dir"

sh "$repo_dir/packaging/stage-package.sh" all "$stage_dir" "$binary_dir"

find "$stage_dir" -type f -printf '/%P\n' | LC_ALL=C sort >"$work_dir/actual-files"
cat >"$work_dir/expected-files" <<'EOF'
/usr/bin/noire
/usr/bin/noirectl
/usr/bin/noired
/usr/lib/systemd/user/noire.service
/usr/share/applications/io.github.rayan6ms.Noire.desktop
/usr/share/bash-completion/completions/noirectl
/usr/share/dbus-1/interfaces/io.github.rayan6ms.Noire.Noire1.xml
/usr/share/dbus-1/services/io.github.rayan6ms.Noire.Noire1.service
/usr/share/doc/noire-daemon/config-v1.toml
/usr/share/fish/vendor_completions.d/noirectl.fish
/usr/share/icons/hicolor/scalable/apps/io.github.rayan6ms.Noire.svg
/usr/share/licenses/noire-daemon/LICENSE
/usr/share/licenses/noire-ui/LICENSE
/usr/share/licenses/noire-ui/icon-LICENSE
/usr/share/man/man1/noire.1
/usr/share/man/man1/noirectl.1
/usr/share/man/man1/noired.1
/usr/share/metainfo/io.github.rayan6ms.Noire.metainfo.xml
/usr/share/zsh/site-functions/_noirectl
EOF
diff -u "$work_dir/expected-files" "$work_dir/actual-files"

for binary in noire noirectl noired; do
    [ "$(stat -c '%a' "$stage_dir/usr/bin/$binary")" = 755 ]
    "$stage_dir/usr/bin/$binary" --version | grep -F "$binary 1.1.0" >/dev/null
done
if find "$stage_dir/usr/share" -type f ! -perm 0644 | grep -q .; then
    echo "Package data contains a file without mode 0644" >&2
    exit 1
fi
if find "$stage_dir" -type f | grep -Eq '/(home|root)/'; then
    echo "Package payload must not contain user-home files" >&2
    exit 1
fi

grep -Fx 'Name=io.github.rayan6ms.Noire.Noire1' \
    "$stage_dir/usr/share/dbus-1/services/io.github.rayan6ms.Noire.Noire1.service" >/dev/null
grep -Fx 'SystemdService=noire.service' \
    "$stage_dir/usr/share/dbus-1/services/io.github.rayan6ms.Noire.Noire1.service" >/dev/null
activation_name=$(sed -n 's/^Name=//p' \
    "$stage_dir/usr/share/dbus-1/services/io.github.rayan6ms.Noire.Noire1.service")
[ "$(basename "$stage_dir/usr/share/dbus-1/services/io.github.rayan6ms.Noire.Noire1.service")" = \
    "$activation_name.service" ]
grep -Fx 'BusName=io.github.rayan6ms.Noire.Noire1' \
    "$stage_dir/usr/lib/systemd/user/noire.service" >/dev/null

if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate \
        "$stage_dir/usr/share/applications/io.github.rayan6ms.Noire.desktop"
fi
if command -v appstreamcli >/dev/null 2>&1; then
    appstreamcli validate --no-net \
        "$stage_dir/usr/share/metainfo/io.github.rayan6ms.Noire.metainfo.xml"
fi
formats=stage
if command -v dpkg-deb >/dev/null 2>&1; then
    deb_dir="$artifact_dir/deb"
    NOIRE_PACKAGE_ALLOW_TEST_BINARIES=1 \
        sh "$repo_dir/packaging/debian/build.sh" 1.1.0-1 amd64 "$binary_dir" "$deb_dir"
    [ "$(find "$deb_dir" -type f -name '*.deb' | wc -l)" -eq 3 ]
    dpkg-deb --field "$deb_dir/noire-daemon_1.1.0-1_amd64.deb" Package | grep -Fx noire-daemon >/dev/null
    dpkg-deb --field "$deb_dir/noire-ui_1.1.0-1_amd64.deb" Depends | grep -F 'noire-daemon (= 1.1.0-1)' >/dev/null
    dpkg-deb --field "$deb_dir/noire_1.1.0-1_amd64.deb" Depends | grep -F 'noire-ui (= 1.1.0-1)' >/dev/null
    dpkg-deb --contents "$deb_dir/noire-daemon_1.1.0-1_amd64.deb" | grep -F './usr/bin/noired' >/dev/null
    if dpkg-deb --contents "$deb_dir/noire-daemon_1.1.0-1_amd64.deb" | grep -F '/gpui' >/dev/null; then
        echo "Headless Debian package unexpectedly contains GPUI files" >&2
        exit 1
    fi
    formats="$formats,deb"
fi

if command -v rpmbuild >/dev/null 2>&1 && command -v rpm >/dev/null 2>&1; then
    rpm_dir="$artifact_dir/rpm"
    NOIRE_PACKAGE_ALLOW_TEST_BINARIES=1 \
        sh "$repo_dir/packaging/rpm/build.sh" 1.1.0 x86_64 "$binary_dir" "$rpm_dir"
    [ "$(find "$rpm_dir" -type f -name '*.rpm' | wc -l)" -eq 3 ]
    daemon_rpm=$(find "$rpm_dir" -type f -name 'noire-daemon-*.rpm' -print -quit)
    ui_rpm=$(find "$rpm_dir" -type f -name 'noire-ui-*.rpm' -print -quit)
    meta_rpm=$(find "$rpm_dir" -type f -name 'noire-[0-9]*.rpm' -print -quit)
    [ -n "$daemon_rpm" ] && [ -n "$ui_rpm" ] && [ -n "$meta_rpm" ]
    rpm -qp --qf '%{NAME}\n' "$daemon_rpm" | grep -Fx noire-daemon >/dev/null
    rpm -qp --requires "$ui_rpm" | grep -F 'noire-daemon' >/dev/null
    rpm -qp --requires "$meta_rpm" | grep -F 'noire-ui' >/dev/null
    rpm -qlp "$daemon_rpm" | grep -Fx '/usr/bin/noired' >/dev/null
    if rpm -qlp "$daemon_rpm" | grep -F '/gpui' >/dev/null; then
        echo "Headless RPM unexpectedly contains GPUI files" >&2
        exit 1
    fi
    if command -v rpmlint >/dev/null 2>&1; then
        rpmlint "$repo_dir/packaging/rpm/noire.spec"
    fi
    formats="$formats,rpm"
fi

sh "$repo_dir/.github/scripts/run_appimage_apprun_smoke.sh"

echo "NOIRE_PHASE9_PACKAGE content=pass metadata=pass split=pass appimage_migration=pass formats=$formats"
