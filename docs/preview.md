# File previews in Lig

Lig can show the selected local file or folder beside the completion list. Enable previews in the user TOML file printed by `lig config path`, then start a new Lig shell. `lig preview` still shows the static completion theme sample.

## Configuration examples

Each example is a standalone configuration excerpt. Merge its tables into your existing `config.toml`.

### Fully native

```toml
[preview]
enabled = true
layout = "auto"
width = "40%"
height = "60%"
max_width = "auto"
max_height = "auto"
adaptive_size = true

[preview.text]
max_lines = 200
line_numbers = true
syntax_highlighting = true

[preview.image]
image_protocol = "auto"
```

The native defaults show text, source code, images including WebP, archive entries and a directory tree up to three levels below the selected folder. The preview title uses the completion list's file or folder icon, including custom `[icons]` overrides and the configured icon mode. The tree uses the same icons, icon overrides and hidden-file setting as the completion list. It shows symlinks without expanding their contents. Lig shows an animated spinner while the selected preview is loading. When a preview cannot be generated, Lig shows file metadata with the modification date and time in the terminal locale. Contextual `git add`, `git diff` and `git restore` previews use Git when available and otherwise fall back to the file.

### Archives and media

ZIP, ZIP64, TAR, TAR.GZ and standalone GZIP have built-in readers. Lig recognizes common archive, audio and video formats so the corresponding configured backend can handle them. With a configured MediaInfo backend, WebM previews include media details. Otherwise Lig shows file metadata with the modification date in the terminal locale.

Add these tables to your config to use separately installed command-line tools.

```toml
[preview.media]
backend = "command"
command = ["mediainfo", "--Output=JSON", "--File_CheckSideCarFiles=0", "--File_TestContinuousFileNames=0", "--", "{file}"]
output = "mediainfo_json"
fallback = "native"

[preview.archive]
backend = "command"
command = ["7zz", "l", "-slt", "-sccUTF-8", "-bd", "-spd", "--", "{file}"]
output = "seven_zip_list"
fallback = "native"
```

Use the CLI name on your shell's PATH or its absolute path. On Windows, use the MediaInfo **CLI** executable and a path such as `C:/Program Files/7-Zip/7z.exe` for 7-Zip. Some installations name the 7-Zip CLI `7z`. With `fallback = "native"`, a failed command falls back to the built-in reader when available and to file metadata otherwise. Rules such as `[preview.rules.7z]` or `[preview.rules.mkv]` take precedence over the category tables.

MediaInfo output shows the container, duration, size, bitrate and available video, audio and subtitle track details. The 7-Zip listing shows members, directories, available uncompressed sizes and encryption indicators. Archive coverage depends on the CLI version and its codecs. A compressed TAR may show the inner TAR as one member. You can use `bsdtar -tvf` with `output = "plain"` in a rule for that extension if your installed bsdtar supports it.

Configured commands use the existing timeout and capture limits. The default configurations use Lig's built-in readers. Lig runs each configured command with null stdin and sanitizes its output before rendering.

### Bat and Chafa

```toml
[preview]
enabled = true

[preview.text]
backend = "command"
command = ["bat", "--paging=never", "--color=always", "--style=numbers", "--line-range", ":{max_lines}", "--", "{file}"]
output = "ansi"
fallback = "native"

[preview.image]
backend = "command"
command = ["chafa", "--format=symbols", "--animate=off", "--probe=off", "--size={width}x{height}", "--", "{file}"]
output = "ansi"
fallback = "native"
```

Use the program name available on your PATH. Some installations use `batcat`.

### Shell scripts

```toml
[preview]
enabled = true

[preview.text]
backend = "shell"
interpreter = "sh"
script = '''
case "$LIG_PREVIEW_EXT" in
  json) exec jq --color-output . -- "$LIG_PREVIEW_FILE" ;;
  *) exec bat --paging=never --color=always --style=numbers -- "$LIG_PREVIEW_FILE" ;;
esac
'''
output = "ansi"
fallback = "native"
```

On Windows, PowerShell uses the same environment variables.

```toml
[preview]
enabled = true

[preview.text]
backend = "shell"
interpreter = "powershell.exe"
script = '''
$ErrorActionPreference = 'Stop'
Get-Content -LiteralPath $env:LIG_PREVIEW_FILE -TotalCount ([int]$env:LIG_PREVIEW_MAX_LINES)
'''
output = "plain"
fallback = "native"
```

### Mixed native and external

```toml
[preview]
enabled = true

[preview.rules.json]
backend = "command"
command = ["jq", "--color-output", ".", "--", "{file}"]
fallback = "native"

[preview.rules.md]
backend = "command"
command = ["glow", "{file}"]
output = "plain"
fallback = "native"
```

All other extensions use their category defaults. A rule can also name `"tar.gz"`. When absent, the `gz` rule applies to `.tar.gz`.

### Floating with automatic placement

```toml
[preview]
enabled = true
layout = "floating"
width = "45%"
height = "60%"
max_width = "auto"
max_height = "auto"
margin = 1
```

The floating panel chooses the side with more space above or below the active prompt, then anchors to its right edge. A floating image uses its remembered size when it fits and the largest restorable rectangle when space is tight. Rows already on screen when Lig starts stay visible. A full clear of the normal screen makes those rows available to the panel. Lig can reduce margins and temporarily compact the list to make room. Very small areas use a borderless preview. Both the pane and the list recover their usual sizes when space returns.

Inline and floating previews fit the width and height of the visible content by default. Images use the terminal cell proportions to keep their aspect ratio, including when a terminal image protocol is active. A long title is clipped to the frame.

### Inline, opening on selection

```toml
[preview]
enabled = true
show_on = "on_hover"
on_hover_open_delay = 500
animation = "fade"
animation_duration_ms = 300
layout = "inline"
width = "45%"
height = "60%"
max_width = "auto"
max_height = "auto"

[preview.keys]
toggle = "alt-p"
toggle_layout = "alt-f"
resize_floating = "alt-z"
up = "alt-up"
down = "alt-down"
page_up = "page-up"
page_down = "page-down"
```

An explicit inline pane sits flush beside the completion list. Lig narrows the list when necessary and uses the regular compact layout if the two panes cannot fit. With `layout = "auto"`, Lig can also choose a stacked or floating placement. The list and preview fit into the available terminal rows without scrolling the terminal.

- **Alt+P** opens or hides the preview. Closing it restores the regular completion layout.
- **Alt+F** switches between inline and floating for the session while keeping the current visibility.
- **Alt+Z** switches a visible floating text-file preview between its compact size and the largest available rectangle, subject to the preview margins, `max_width`, and `max_height`. Directory, archive, and metadata previews expand in height. Images cycle through the configured size, 66%, and 90%.
- **Alt+Z** also widens an expanded floating Git diff when there is room for at least 80 content columns and `delta` is available on the shell's `PATH`. Lig uses a unified diff in compact and inline panes and when a side-by-side diff is unavailable. Git and `delta` run with paging disabled for previews.

Zoom resets when the selection changes or the completion list closes. For the same selection, it survives terminal resizes and hiding and reopening the preview.

`show_on` selects when the preview appears. `always` shows it for each selected file or folder; the toggle can hide it for the current selection, and the next selection shows it again. `key_toggle` starts hidden and shows only on an explicit open or toggle; closing it or changing selection hides it. `on_hover` starts a timer for each file or folder selection, shows the preview if the selection remains unchanged until `on_hover_open_delay`, and closes it when selection changes. A manual close suppresses reopening for that same selection. The default is `always`, with an `on_hover_open_delay` of `500` milliseconds. An empty key value disables that binding. Set `preview.keys.open` and `preview.keys.close` for dedicated bindings. Existing `[keys]` bindings and explicit preview shortcuts take precedence over the new defaults. Ctrl+P remains the default previous-candidate key.

`animation` selects `none`, `fade`, or `reveal`; `none` is the default. `animation_duration_ms` sets the transition duration; `0` skips the transition. The transitions do not block input or completion work.

## Display and resource settings

| Setting | Default | Behavior |
|---|---|---|
| `enabled`, `show_on` | `false`, `"always"` | Enable previews and choose `always`, `key_toggle` or `on_hover` visibility. |
| `on_hover_open_delay` | `500` | Wait this many milliseconds before showing a stable selection in `on_hover` mode. Maximum `30000`. |
| `animation` | `"none"` | Preview entrance effect: `"none"`, `"fade"`, or `"reveal"`. |
| `animation_duration_ms` | `300` | Duration of `fade` and `reveal` transitions in milliseconds; `0` skips the transition. Maximum `30000`. |
| `layout` | `"auto"` | Choose automatic, inline or floating placement. Lig selects the position automatically; inline uses side placements first. |
| `width`, `height` | `"40%"`, `"60%"` | Preferred outer size in cells or percentage of the terminal. `"auto"` is not accepted. |
| `max_width`, `max_height` | `"auto"`, `"auto"` | Cap the outer size in cells or percentage of the terminal. `"auto"` uses all available space. Frames are capped at 256 columns and 128 rows. |
| `min_width`, `min_height` | `28`, `5` | Minimum dimensions when finding inline space and preferred dimensions for floating placement. A fitted preview can draw a smaller frame. |
| `margin` | `1` | Space between a floating pane and the list. |
| `screen_margin` | `10` | Minimum distance in cells between a floating pane and each terminal edge. Maximum `40`; smaller terminals constrain the pane to the available area. |
| `adaptive_size` | `true` | Fit the frame to native content and configured command output. Commands get the available viewport before fitting. Set to `false` to keep the allocated pane size. Floating images still fit the frame horizontally. |
| `preserve_prompt` | `true` | Apply the configured margin between a floating pane and the prompt. Active prompt and input rows are always protected. |
| `show_title` | `true` | Show the filename in the preview frame. |
| `scroll_step` | `3` | Lines per wheel or line-scroll key. |
| `mouse` | `true` | Scroll when the wheel is inside the visible preview. Terminals can require a modifier to select or use terminal scrollback while Lig owns mouse reporting. |
| `[preview.text].max_lines` | `200` | Limit text preview rows. Other providers that render text can set their own `max_lines` under `[preview.image]`, `[preview.archive]`, `[preview.media]`, `[preview.directory]` or `[preview.rules.<extension>]`. |
| `max_bytes` | `262144` | Bound the file prefix for native text and stdout from configured commands. Stderr is discarded after counting up to 64 KiB. |
| `timeout_ms`, `cache_entries` | `2000`, `16` | External process deadline and the in-memory LRU entry cap. Cache storage is also limited to 16 MiB. |
| `[preview.text].line_numbers`, `syntax_highlighting` | `true`, `true` | Native text styling. Very long lines and the tail of large files use plain text. |
| `[preview.image].image_protocol` | `"auto"` | Detect Kitty or iTerm2. Set `"cells"` to force quadrants or `"half_blocks"` to use simpler glyphs with older fonts. |

Lig accepts the legacy `preserve_completion` and `show_controls` settings for compatibility. Inline sizing is automatic and the popup has no preview key legend.

With `adaptive_size = true`, Lig fits the drawn frame to the rendered content, within the allocated size and `max_width`/`max_height`. With `false`, it keeps the allocated preview dimensions; image content still fits its frame while preserving its aspect ratio. Configured commands receive the available content viewport before Lig applies content fitting.

`command` passes separate process arguments. `shell` runs the literal script from the user configuration through `sh`, PowerShell or an explicitly configured interpreter. POSIX-compatible interpreters receive `-c`. The two backends expose the same values; `{max_lines}` uses the selected provider's limit.

| Argument placeholder | Environment variable |
|---|---|
| `{file}` | `LIG_PREVIEW_FILE` |
| `{width}` | `LIG_PREVIEW_WIDTH` |
| `{height}` | `LIG_PREVIEW_HEIGHT` |
| `{max_lines}` | `LIG_PREVIEW_MAX_LINES` |
| `{extension}` | `LIG_PREVIEW_EXT` |

The width and height exclude the frame. Paths and placeholders remain arguments or environment values. Shell code comes only from the user-level Lig configuration. `output = "ansi"` keeps supported styles and colors. `output = "plain"` also removes styling. Both modes discard cursor commands, terminal mode changes, hyperlinks and raw graphics. Errors appear in the Lig debug log with `LIG_LOG=1`. `fallback = "metadata"` skips the native backend after an external failure.

## Formats and terminal support

- Native text supports UTF-8 and BOM-marked UTF-16 in either byte order, including files saved by Windows PowerShell. The highlighting grammars cover shell, Python, Rust, Java, JavaScript, TypeScript, C/C++, Markdown, JSON and JSONC, TOML, YAML, INI, XML, HTML and CSS.
- Lig checks file signatures before consulting extensions and tests unknown content for binary data. The same detection runs on Windows, macOS and Linux. On Unix the timestamp follows `LC_ALL`, `LC_TIME` or `LANG` in that order. On Windows, Lig uses a supplied locale when supported and falls back to the user's regional date and time formats. [Windows date formatting](https://learn.microsoft.com/en-us/windows/win32/api/datetimeapi/nf-datetimeapi-getdateformatex).
- PNG, JPEG (including progressive, `.jpg`, `.jpeg`, `.jpe` and `.jfif`), GIF, WebP and BMP use the native image decoder. GIF displays its first frame. The decoder bounds input to 32 MiB and the raster to eight million pixels. The native graphics thumbnail is at most 1024 pixels on either side, with further limits on total pixels and encoded size.
- ZIP and ZIP64 read only entry metadata from the central directory. TAR and TAR.GZ stream headers with an 8 MiB scan limit, so a large member can leave a partial listing. GZIP shows header metadata and the last member's size modulo 4 GiB when available. Archive paths remain display text. For 7z, RAR or other formats, configure a listing command.
- In `auto` mode, Lig uses inline graphics after a terminal capability reply. Direct iTerm2 sessions prefer iTerm2 inline images. tmux uses native Kitty graphics when the client exposes `kittygraphics` and tmux answers the Kitty probe. Other cases use a 2×2 glyph mosaic with two colors per cell, antialiasing and RGB, indexed or basic terminal colors. Lig uses the reported cell proportions when available and falls back to 1:2. External preview commands continue to supply plain or sanitized ANSI text. [Kitty protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/), [iTerm2 protocol](https://iterm2.com/documentation-images.html), [tmux client capabilities](https://man.openbsd.org/tmux.1).

See the [architecture and validation notes](preview-assessment.md) for the upstream integration and remaining limits.
