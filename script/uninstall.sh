#!/usr/bin/env sh
set -eu

# Uninstalls LightCode that was installed using the install.sh script

check_remaining_installations() {
    platform="$(uname -s)"
    if [ "$platform" = "Darwin" ]; then
        # Check for any LightCode variants in /Applications
        remaining=$(ls -d /Applications/LightCode*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    else
        # Check for any LightCode variants in ~/.local
        remaining=$(ls -d "$HOME/.local/lightcode"*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    fi
}

prompt_remove_preferences() {
    printf "Do you want to keep your LightCode preferences? [Y/n] "
    read -r response
    case "$response" in
    [nN] | [nN][oO])
        rm -rf "$HOME/.config/lightcode"
        echo "Preferences removed."
        ;;
    *)
        echo "Preferences kept."
        ;;
    esac
}

main() {
    platform="$(uname -s)"
    channel="${GRAM_CHANNEL:-stable}"

    if [ "$platform" = "Darwin" ]; then
        platform="macos"
    elif [ "$platform" = "Linux" ]; then
        platform="linux"
    else
        echo "Unsupported platform $platform"
        exit 1
    fi

    "$platform"

    echo "LightCode has been uninstalled"
}

linux() {
    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    # Remove the app directory
    rm -rf "$HOME/.local/lightcode$suffix.app"

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/lightcode"

    app_id="dev.lightcode.editor"
    if [ "$channel" = "dev" ]; then
        app_id="dev.lightcode.editor-dev"
    fi

    # Remove desktop integration installed by the current installer.
    rm -f "$HOME/.local/share/applications/$app_id.desktop"
    rm -f "$HOME/.local/share/icons/hicolor/scalable/apps/$app_id.svg"
    rm -f "$HOME/.local/share/icons/hicolor/symbolic/apps/$app_id-symbolic.svg"

    # Refresh icon lookup without creating a missing theme directory.
    if [ -d "$HOME/.local/share/icons/hicolor" ]; then
        touch "$HOME/.local/share/icons/hicolor"
    fi

    if [ -d "$HOME/.local/share/applications" ] && command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$HOME/.local/share/applications" ||\
            printf '%s\n' 'Warning: could not refresh the desktop database.' >&2
    fi

    # Remove the database directory for this channel
    rm -rf "$HOME/.local/share/lightcode/db/0-$suffix"

    # Remove socket file
    rm -f "$HOME/.local/share/lightcode/lightcode-$suffix.sock"

    # Remove the entire LightCode directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/.local/share/lightcode"
        prompt_remove_preferences
    fi

    rm -rf "$HOME"/.lightcode_server
}

macos() {
    app="LightCode.app"
    db_suffix="stable"
    app_id="dev.lightcode.editor"
    case "$channel" in
    dev)
        app="LightCode Dev.app"
        db_suffix="dev"
        app_id="dev.lightcode.editor-dev"
        ;;
    esac

    # Remove the app bundle
    if [ -d "/Applications/$app" ]; then
        rm -rf "/Applications/$app"
    fi

    # Remove only a CLI symlink pointing into this LightCode application.
    if [ -L /usr/local/bin/lightcode ]; then
        cli_target=$(readlink /usr/local/bin/lightcode)
        case "$cli_target" in
        "/Applications/$app/Contents/MacOS/cli")
            rm -f /usr/local/bin/lightcode
            ;;
        esac
    fi
    rm -f "$HOME/.local/bin/lightcode"

    # Remove the database directory for this channel
    rm -rf "$HOME/Library/Application Support/LightCode/db/0-$db_suffix"

    # Remove app-specific files and directories
    rm -rf "$HOME/Library/Application Support/com.apple.sharedfilelist/com.apple.LSSharedFileList.ApplicationRecentDocuments/$app_id.sfl"*
    rm -rf "$HOME/Library/Caches/$app_id"
    rm -rf "$HOME/Library/HTTPStorages/$app_id"
    rm -rf "$HOME/Library/Preferences/$app_id.plist"
    rm -rf "$HOME/Library/Saved Application State/$app_id.savedState"

    # Remove the entire LightCode directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/Library/Application Support/LightCode"
        rm -rf "$HOME/Library/Logs/LightCode"

        prompt_remove_preferences
    fi

    rm -rf "$HOME"/.lightcode_server
}

main "$@"
