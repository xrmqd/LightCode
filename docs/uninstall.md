# Uninstalling LightCode

## macOS

1. Quit LightCode.
2. Move `LightCode.app` from Applications to the Trash.
3. If you installed the CLI, remove `/usr/local/bin/lightcode` only after
   checking that it is a symlink to the LightCode application you removed.

To remove LightCode's settings and data as well, remove these folders if present:

- `~/.config/lightcode`
- `~/Library/Application Support/LightCode`
- `~/Library/Logs/LightCode`
- `~/Library/Caches/LightCode`
- `~/Library/Caches/dev.lightcode.editor`
- `~/Library/Saved Application State/dev.lightcode.editor.savedState`

Development builds may also use the `dev.lightcode.editor-dev` identifier.
Custom data directories supplied with `--user-data-dir` must be managed separately.

These locations belong to LightCode. Gram's settings, data and installation
have separate names and do not need to be removed.

## Other platforms

This fork currently publishes only macOS builds. If you built LightCode yourself
for another platform, remove it using the installation or package method you used.
