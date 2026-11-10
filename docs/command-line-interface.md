# LightCode Command-line Interface

On macOS, use **LightCode → Install CLI** to create a `lightcode` symlink at
`/usr/local/bin/lightcode` pointing to the CLI inside the installed application.

Use `lightcode --help` for all available options. Examples:

- Open a new window: `lightcode`.
- Open a project: `lightcode /path/to/project`.
- Open a file in a new window: `lightcode -n /path/to/file`.
- Read standard input: `ps axf | lightcode -`.
- Show logs in the terminal: `lightcode --foreground`.
- Show the installed version and application path: `lightcode --version`.
- Select an application explicitly: `lightcode --lightcode /Applications/LightCode.app`.

The `--gram` option is retained as a compatibility alias for `--lightcode`.
Internal Rust packages and the executable inside the application bundle retain
upstream names; the installed command is `lightcode`.
