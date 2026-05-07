#!/usr/bin/env sh
set -eu

app_id="term41"
app_name="term41"
icon_name="term41"

usage() {
    cat <<EOF
Usage: $0 [OPTIONS]

Install a user-local .desktop launcher and hicolor icon assets for term41.

Options:
    --exec COMMAND     Exact Exec= command to write into the .desktop file.
                       Defaults to an installed term41 binary when found.
    --data-home DIR    XDG data directory to install into.
                       Defaults to XDG_DATA_HOME or ~/.local/share.
    --uninstall        Remove the installed desktop file and icon assets.
    -h, --help         Show this help text.
EOF
}

die() {
    printf '%s\n' "error: $*" >&2
    exit 1
}

info() {
    printf '%s\n' "$*"
}

repo_root() {
    script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
    dirname -- "$script_dir"
}

default_data_home() {
    if [ "${XDG_DATA_HOME:-}" != "" ]; then
        printf '%s\n' "$XDG_DATA_HOME"
        return
    fi

    if [ "${HOME:-}" = "" ]; then
        die "HOME is not set; pass --data-home explicitly"
    fi

    printf '%s\n' "$HOME/.local/share"
}

default_exec_command() {
    root=$1

    if command -v "$app_id" >/dev/null 2>&1; then
        command -v "$app_id"
        return
    fi

    if [ "${HOME:-}" != "" ] && [ -x "$HOME/.cargo/bin/$app_id" ]; then
        printf '%s\n' "$HOME/.cargo/bin/$app_id"
        return
    fi

    if [ -x "$root/target/release/$app_id" ]; then
        printf '%s\n' "$root/target/release/$app_id"
        return
    fi

    printf '%s\n' "$app_id"
}

copy_asset() {
    source=$1
    target=$2

    if command -v install >/dev/null 2>&1; then
        install -m 0644 -- "$source" "$target"
        return
    fi

    cp -- "$source" "$target"
    chmod 0644 "$target"
}

write_desktop_file() {
    target=$1
    exec_command=$2

    cat > "$target" <<EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=$app_name
GenericName=Terminal Emulator
Comment=GPU-accelerated terminal emulator
Exec=$exec_command
Icon=$icon_name
Terminal=false
Categories=System;TerminalEmulator;
Keywords=shell;prompt;command;terminal;console;
StartupNotify=true
EOF

    chmod 0644 "$target"
}

refresh_caches() {
    data_home=$1

    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$data_home/applications" >/dev/null 2>&1 || true
    fi

    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t "$data_home/icons/hicolor" >/dev/null 2>&1 || true
    fi

    if command -v xdg-icon-resource >/dev/null 2>&1; then
        xdg-icon-resource forceupdate >/dev/null 2>&1 || true
    fi
}

install_desktop_assets() {
    root=$1
    data_home=$2
    exec_command=$3

    icon_svg="$root/resources/icon.svg"
    icon_png="$root/resources/icon.png"

    [ -f "$icon_svg" ] || die "missing icon asset: $icon_svg"
    [ -f "$icon_png" ] || die "missing icon asset: $icon_png"

    desktop_dir="$data_home/applications"
    svg_icon_dir="$data_home/icons/hicolor/scalable/apps"
    png_icon_dir="$data_home/icons/hicolor/128x128/apps"

    mkdir -p -- "$desktop_dir" "$svg_icon_dir" "$png_icon_dir"

    copy_asset "$icon_svg" "$svg_icon_dir/$icon_name.svg"
    copy_asset "$icon_png" "$png_icon_dir/$icon_name.png"
    write_desktop_file "$desktop_dir/$app_id.desktop" "$exec_command"

    if command -v desktop-file-validate >/dev/null 2>&1; then
        desktop-file-validate "$desktop_dir/$app_id.desktop"
    fi

    refresh_caches "$data_home"

    info "Installed $desktop_dir/$app_id.desktop"
    info "Installed $svg_icon_dir/$icon_name.svg"
    info "Installed $png_icon_dir/$icon_name.png"
}

uninstall_desktop_assets() {
    data_home=$1

    desktop_file="$data_home/applications/$app_id.desktop"
    svg_icon="$data_home/icons/hicolor/scalable/apps/$icon_name.svg"
    png_icon="$data_home/icons/hicolor/128x128/apps/$icon_name.png"

    rm -f -- "$desktop_file" "$svg_icon" "$png_icon"
    refresh_caches "$data_home"

    info "Removed $desktop_file"
    info "Removed $svg_icon"
    info "Removed $png_icon"
}

main() {
    exec_command=""
    data_home=""
    uninstall="false"

    while [ "$#" -gt 0 ]; do
        case "$1" in
            --exec)
                shift
                [ "$#" -gt 0 ] || die "--exec requires a command"
                exec_command=$1
                ;;
            --data-home)
                shift
                [ "$#" -gt 0 ] || die "--data-home requires a directory"
                data_home=$1
                ;;
            --uninstall)
                uninstall="true"
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                die "unknown option: $1"
                ;;
        esac
        shift
    done

    root=$(repo_root)
    data_home=${data_home:-$(default_data_home)}

    if [ "$uninstall" = "true" ]; then
        uninstall_desktop_assets "$data_home"
        exit 0
    fi

    exec_command=${exec_command:-$(default_exec_command "$root")}
    install_desktop_assets "$root" "$data_home" "$exec_command"
}

main "$@"
