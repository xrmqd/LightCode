# LightCode Command-line Interface

On macOS, use **LightCode → Install lcode Command** to create a `lcode` symlink at
`/usr/local/bin/lcode` pointing to the CLI inside the installed application.

Use `lcode --help` for all available options. Examples:

- Open a new window: `lcode`.
- Open a project: `lcode /path/to/project`.
- Open a file in a new window: `lcode -n /path/to/file`.
- Read standard input: `ps axf | lcode -`.
- Show logs in the terminal: `lcode --foreground`.
- Show the installed version and application path: `lcode --version`.
- Select an application explicitly: `lcode --lcode /Applications/LightCode.app`.

The `--gram` and `--lightcode` options are retained as compatibility aliases for `--lcode`.
Internal Rust packages and the executable inside the application bundle retain
upstream names; the installed command is `lcode`.
