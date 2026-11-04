# Tabby Feature Matrix

Status: **current** (inventory) · Reference: Tabby v1.0.238, released 2026-10-07 · Owner: mouv ·
Applies to: Subak · Created in Step 03

This matrix lists Tabby's user-facing features and Subak's planned answer to each one. It is the
tracking document for `SUBAK-FR-061`. Tabby parity is a long-term direction (`P2`, milestone M3),
not an MVP promise. The M2 MVP includes only the basic subset in `SUBAK-FR-033` and
`SUBAK-FR-035`. Subak follows the [Kim philosophy](KIM_PHILOSOPHY.md) and the
[Subak philosophy](SUBAK_PHILOSOPHY.md). Requirement IDs refer to
[PRODUCT_REQUIREMENTS.md](PRODUCT_REQUIREMENTS.md).

**Nothing in this matrix is implemented yet.** Every row is `planned`, `excluded` or `blocked`,
and every Evidence cell is `—`. **Step 32 re-audits** the matrix against the Tabby release that is
current at that time, and updates the Reference line and the [audit history](#audit-history).

## Sources and method

Sources were fetched on 2026-11-03.

| Code | Source | What was used |
| --- | --- | --- |
| **R** | Tabby `README.md` on `master` (raw.githubusercontent.com/Eugeny/tabby/master/README.md) | The headline feature list, the "Terminal features", "SSH Client", "Serial Terminal", "Portable", "Plugins" and "Themes" sections. Quotes are verbatim. |
| **N** | Latest GitHub release: tag `v1.0.238`, published 2026-10-07T06:43:54Z (api.github.com/repos/Eugeny/tabby/releases/latest) | Release notes and the list of release assets (installers, portable zips, platforms) |
| **S** | Tabby source on `master` at the fetch date, read for setting names only: config defaults (`tabby-core/src/configDefaults.yaml`, `tabby-terminal/src/config.ts`), hotkey IDs, SSH, serial and Telnet profile options, shell detectors (`tabby-electron/src/shells/`), and package names | Named settings (`copyAsHTML`), hotkeys (`split-right`) and modules (`tabby-uac`). `master` may be slightly newer than v1.0.238. |
| **K** | Own knowledge of Tabby, not confirmed in the fetched text | Marked **K** wherever it is used. Step 32 must confirm or correct it. |

The website https://tabby.sh is rendered client-side. The fetch returned only the page title
("Tabby - a terminal for a more modern age") and no feature text, so no row relies on it. The
plugin API docs (docs.tabby.sh) are about development and were not inventoried.

## Legend

**Status** (exactly one per row):

| Status | Meaning |
| --- | --- |
| `planned` | Subak intends to provide it at the Target Step. Nothing exists yet. |
| `partial` | Some of it ships, with evidence. The approach cell says what is missing. |
| `implemented` | It ships, with evidence (see [How evidence is recorded](#how-evidence-is-recorded)). *(Not used yet.)* |
| `excluded` | Deliberately not built. The approach cell gives the rationale. |
| `blocked` | Wanted, but cannot proceed until the named dependency is resolved. |

**Target Step**: Step 17 tabs and shortcuts · Step 18 floating icon and terminal manager · Step 19
search, clipboard, links · Step 20 themes, fonts, basic profiles, settings · Step 22 split panes ·
Step 29 persistence (remember tabs) · Step 32 Tabby audit refresh · Step 33 shell profiles and
productivity · Step 34 SSH and credentials · Step 35 SFTP, jump hosts, forwarding, Zmodem, login
scripts · Step 36 serial, Telnet, notifications, session organization, config import. A few rows
point at foundation steps named in the accepted ADRs: Step 13 (attach, replay, backpressure),
Step 15 (VT emulator fidelity), Step 21 (automatic naming), Steps 24–25 (moving panes,
`join-pane`) and Step 37 (MCP). `M1 (Kim)` means the
Kim manager, `M4` means the Linux milestone, and `—` means not scheduled.

Each row has a permanent ID (`CORE-01`) so that commits, tests and later audits can cite it.

## 1. Terminal core

### 1.1 Emulation and performance

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `CORE-01` VT220 emulation with extensions | R: "A VT220 terminal + various extensions" | Host-side headless VT emulator per pane, whose grid the GUI renders: one model in `subak-term` for the Host and every client view ([TERMINAL_EMULATOR](TERMINAL_EMULATOR.md)). *Partial:* the emulator and its vttest-like conformance fixtures exist; the Step 16 window (`subak-gui`) shows it; tabs are Step 17 | partial | Step 15 | T: `crates/subak-term/tests/conformance.rs` (`vttest_cursor_movements_border_and_frame` and 15 more) (Step 15, commits db12228, c26b026, fe0a286) |
| `CORE-02` xterm extensions: 256 colors, truecolor, mouse, alternate screen, OSC titles | S `term: 'xterm-256color'`; K | Same emulator: SGR truecolor, mouse modes 9/1000/1002/1003 with 1005/1006/1015/1016, DECSET 47/1047/1049, OSC 0/2, OSC 8. *Partial:* modelled, encoded and rendered headlessly; the inbox ConPTY of Windows 10 does not pass mouse modes or OSC 8 through (ADR 0036 §5); the Step 16 window shows it | partial | Step 15 | T: `crates/subak-term/tests/conformance.rs::sgr_attributes_and_colours`, `::mouse_modes`, `::alternate_screen_variants`; T: `crates/subak-term/tests/view.rs::mouse_reports_for_every_tracking_mode_and_encoding` (Step 15, commits db12228, c26b026, fe0a286) |
| `CORE-03` Full Unicode including double-width characters | R: "Full Unicode support including double-width characters" | Host emulator and GUI share one width table. Covers Korean/CJK, emoji and combining marks, with system font fallback (KIM-NFR-013, SUBAK-FR-031). *Partial:* shared model and width table; the renderer draws Korean with Malgun Gothic in two cells and emoji with Segoe UI Emoji (monochrome) from real ConPTY output; the Step 16 window shows it, colour emoji need a DirectWrite rasterizer | partial | Steps 15, 20 | T: `crates/subak-term/tests/conformance.rs::unicode_wide_korean_emoji_and_combining`; T: `crates/subak/tests/terminal_render.rs::cmd_renders_korean_wide_glyphs_and_follows_a_resize` (Step 15, commits db12228, c26b026, fe0a286) |
| `CORE-04` Fast output handling | R: "Doesn't choke on fast-flowing outputs" | The Host drains ConPTY into a bounded history ring with backpressure, and the GUI coalesces frames. Throughput is measured, not assumed. | planned | Step 13 | — |
| `CORE-05` Scrollback size | S `scrollbackLines: 25000` | Configurable per-pane history kept in the Host, so it survives GUI close. Saving it to disk is optional and off by default (SUBAK-FR-042). | planned | Step 13 | — |
| `CORE-06` Sixel inline images | S `sixel: true` | Blocked: older inbox ConPTY re-renders output and may drop Sixel data. Step 15 kept the inbox ConPTY for M2 ([ADR 0036](adr/0036-terminal-emulation-and-rendering-path.md) §5: on 19045 even mouse modes and OSC 8 do not pass) and the emulator does not decode images; unblocked by shipping a newer ConPTY and an image-capable renderer | blocked | post-M2 | — |
| `CORE-07` GPU rendering with a fallback | S `frontend: 'xterm-webgl'`, `hacks.disableGPU` | A CPU cell-grid renderer instead of a GPU one ([ADR 0036](adr/0036-terminal-emulation-and-rendering-path.md)): idle 3.4 MiB private against ~107 MiB for egui + glow, measured; frames presented with GDI, so there is no GPU path to fall back from. *Partial:* the renderer and its benchmark exist; the Step 16 window presents it with GDI, idle 3.95 MiB private and 0 CPU (SUBAK_GUI §7) | partial | Step 20 | T: `crates/subak-term/tests/render.rs` (golden frames); `crates/subak-term/examples/term_bench.rs` (D: TERMINAL_EMULATOR §7) (Step 15, commits db12228, c26b026, fe0a286) |

### 1.2 Tabs and windows

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `CORE-08` Tabs | R: "Tabs on any side of the window"; S `next-tab`, `previous-tab` | One tab per Terminal, with `Ctrl+T`, `Ctrl+W` and `Ctrl+Tab` (SUBAK-FR-031, SUBAK-FR-033) | planned | Step 17 | — |
| `CORE-09` Tab bar on any side | R: "Tabs on any side of the window"; S `tabsLocation` | Top by default. Bottom, left and right are a setting. | planned | Step 17 | — |
| `CORE-10` Tab bar options | S `flexTabs`, `hideTabIndex`, `hideCloseButton`, `tabsInFullscreen`, `cycleTabs` | Settings for tab width, index, close button, fullscreen visibility and wrap-around | planned | Step 17 | — |
| `CORE-11` Rename, reorder, jump to tab N | S `rename-tab`, `move-tab-left`, `move-tab-right`, `tab-1` to `tab-20` | Renaming a tab renames the Terminal (SUBAK-FR-013). Reorder by drag or key, and jump with rebindable keys. | planned | Step 17 | — |
| `CORE-12` Duplicate tab | S `duplicate-tab` | Creates a new Terminal with the same profile and cwd. It never clones the running process. | planned | Step 17 | — |
| `CORE-13` Reopen closed tab | S `reopen-tab` | Closing a tab kills its Terminal (SUBAK-FR-014). "Reopen" re-creates it from the profile and cwd with new processes, and says so. A detached Terminal reopens from the session manager (SUBAK-FR-032). | planned | Step 17 | — |
| `CORE-14` Pin tab | S `pin-tab` | Pinned Terminals sort first and need confirmation to close | planned | Step 17 | — |
| `CORE-15` Most-recently-used tab switching | S `next-mru-tab`, `previous-mru-tab`, `toggle-last-tab` | MRU order per client, as optional bindings. Positional `Ctrl+Tab` stays the default (SUBAK-FR-033). | planned | Step 17 | — |
| `CORE-16` Tab and terminal context menus | S `tabContextMenu.ts`, `terminalContextMenu.ts` | Native context menus. Every item is also a command with a CLI equivalent (KIM-FR-013). | planned | Step 17 | — |
| `CORE-17` Restart tab | S `restart-tab` | Kills and respawns the pane process with the same profile, with confirmation when busy | planned | Step 17 | — |
| `CORE-18` Confirm before closing a busy tab | S `ssh.warnOnClose`; K local busy-process warning | Confirms when any pane runs more than an idle shell (SUBAK-FR-014) | planned | Step 17 | — |
| `CORE-19` Last tab closes window | S `lastTabClosesWindow` | Closing a window is always a detach (SUBAK-FR-015). The setting only decides whether the GUI exits when it shows no Terminals. | planned | Step 17 | — |
| `CORE-20` Behavior on session end | K profile option (close, keep open, reconnect) | "Keep pane open on exit" option (Subak philosophy §6.1), plus reconnect for SSH and serial | planned | Step 20 | — |
| `CORE-21` Multiple windows | S `new-window` | Each window is another GUI client of the same Host and can show any set of Terminals (SUBAK-FR-004) | planned | Step 18 | — |
| `CORE-22` Dynamic tab titles | S profile `disableDynamicTitle`; K OSC title | Named after the foreground program or the OSC title until the user renames it. Can be turned off per profile. | planned | Step 21 | — |
| `CORE-23` Tab title prefix/suffix rules | R plugin: "title-control - allows modifying the title of the terminal tabs…" | Title format strings (tmux-style) in config, with no plugin | planned | Step 21 | — |
| `CORE-24` Tab colors and profile icons | S `showTabProfileIcon`; K tab color | Color and icon per profile and per Terminal. Color is never the only signal (KIM-NFR-012). | planned | Step 20 | — |
| `CORE-25` Fullscreen | S `toggle-fullscreen` | Fullscreen per window | planned | Step 17 | — |

### 1.3 Split panes

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `CORE-26` Nested split panes | R: "Split panes"; R: "Multiple nested split panes"; S `split-right`, `split-bottom`, `split-left`, `split-top` | Pane tree owned by the Host. Split with `Ctrl+B %` / `Ctrl+B "` or GUI actions (SUBAK-FR-035). | planned | Step 22 | — |
| `CORE-27` Pane navigation, maximize, resize | S `pane-nav-*`, `pane-maximize`, `pane-increase-vertical`, `paneResizeStep` | tmux keys (arrows, `z`), Tabby-style hotkeys, and mouse drag on splitters | planned | Step 22 | — |
| `CORE-28` Rearrange panes by drag and drop | S `rearrange-panes`, `splitTabDropZone` | Drop a pane on drop zones, and the Host updates the layout | planned | Step 22 | — |
| `CORE-29` Move panes between tabs | S `explode-tab`, `combine-tabs` | Host-side break-pane and join-pane. Processes move and are never restarted. | planned | Steps 24–25 | — |
| `CORE-30` Focus follows mouse | S `focusFollowsMouse` | Optional, off by default | planned | Step 22 | — |
| `CORE-31` Broadcast input to all panes or tabs | S `pane-focus-all`, `focus-all-tabs` | The Host copies input to several panes (tmux `synchronize-panes`). A visible indicator stays on the whole time. Off by default. | planned | Step 22 | — |
| `CORE-32` Pane title labels | S `splitTabPaneLabel` | Optional pane borders showing names | planned | Step 22 | — |

### 1.4 Remembering tabs and panes

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `CORE-33` Remember tabs | R: "Remembers your tabs"; S `recoverTabs: true` | Goes further than Tabby: closing the GUI leaves Terminals running in the Host, and reopening reattaches to the same processes (SUBAK-FR-003). After a reboot, the structure comes back with new processes, marked `restored` (SUBAK-FR-040…043). Commands are never re-run (SUBAK-SEC-002). *Partial:* the Step 16 window reopens the Terminal it showed last (by id, then name) and replays its screen from the Host; several tabs (Step 17) and restore after reboot (Step 29) are not done | partial | Steps 17, 29 | T: `crates/subak/tests/gui_window.rs::closing_or_killing_the_window_never_ends_the_host_or_the_pane` (Step 16, commit f660788) |
| `CORE-34` Remember split layout and cwd | K Tabby recovers split tabs | Layout, names, cwd, profile and non-secret environment are saved atomically (SUBAK-FR-040, SUBAK-SEC-003) | planned | Step 29 | — |
| `CORE-35` Recover connection tabs | S `recoveryProvider.ts` in `tabby-ssh`, `tabby-serial`, `tabby-telnet` | Live connections survive GUI close in the Host. After a reboot they return disconnected, with a Reconnect action. They auto-reconnect only if the profile opts in. | planned | Step 34 | — |

### 1.5 Themes, color schemes and fonts

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `CORE-36` App theme following light/dark | R: "Theming and color schemes"; S `appearance.theme`, `colorSchemeMode` | Kim design system tokens. Follows the Windows light/dark setting. | planned | Step 20 | — |
| `CORE-37` Terminal color schemes, separate light and dark | S `colorScheme`, `lightColorScheme`, `customColorSchemes` | Scheme files paired per mode, with per-profile override | planned | Step 20 | — |
| `CORE-38` Bundled community color schemes | S package `tabby-community-color-schemes` | A curated set shipped as data files with their licenses | planned | Step 20 | — |
| `CORE-39` Minimum contrast ratio | S `minimumContrastRatio: 4` | Contrast floor in the renderer. The default follows KIM-NFR-012 (4.5:1). | planned | Step 20 | — |
| `CORE-40` Bold as bright, palette generation | S `drawBoldTextInBrightColors`, `paletteGenerate` | Renderer options. *Partial:* bold as bright for ANSI 0–7 is on by default (`RenderOptions::bold_is_bright`); palette generation and the settings UI are Step 20 | partial | Step 20 | T: `crates/subak-term/tests/render.rs::colours_attributes_and_golden_frames` (Step 15, commits db12228, c26b026, fe0a286) |
| `CORE-41` Font family, size, weight, line spacing | S `fontSize`, `fontWeight`, `fontWeightBold`, `linePadding` | DirectWrite fonts, set in the settings UI or the config | planned | Step 20 | — |
| `CORE-42` Fallback font | S `fallbackFont` | An explicit fallback chain, then system fallback for Korean/CJK (KIM-NFR-013). *Partial:* the design system's chain (Cascadia Mono → Consolas → D2Coding → Malgun Gothic → Segoe UI Emoji → Segoe UI Symbol) chosen per character by each font's cmap; a user-set chain is Step 20 | partial | Step 20 | T: `crates/subak-term/src/render/gdi.rs::the_chain_skips_missing_families_and_falls_back_by_cmap` (Step 15, commits db12228, c26b026, fe0a286) |
| `CORE-43` Font ligatures | R: "Font ligatures"; S `ligatures: false` | Shaped in the renderer. Off by default, as in Tabby. | planned | Step 20 | — |
| `CORE-44` Zoom in, out, reset | S `zoom-in`, `zoom-out`, `reset-zoom` | Font zoom per window with `Ctrl+=`, `Ctrl+-` and `Ctrl+0`, all rebindable. *Partial:* `Ctrl+Plus`/`Ctrl+Minus`/`Ctrl+0` (and the numpad keys) change the text size 8–36 px, saved in `gui.json` (Step 16); rebinding is Step 17 | partial | Step 20 | T: `crates/subak/tests/gui_window.rs::dpi_resize_minimize_zoom_and_preferences` (Step 16, commit f660788) |
| `CORE-45` Cursor style and blink | S `cursor`, `cursorBlink` | Block, bar or underline. Honors DECSCUSR and reduced motion. *Partial:* the three shapes and the hollow unfocused cursor are drawn from DECSCUSR; blinking (a timer, reduced motion) and the setting are Step 20; the Step 16 window does not blink (no idle timer) | partial | Step 20 | T: `crates/subak-term/tests/render.rs::cursor_shapes_focus_and_visibility` (Step 15, commits db12228, c26b026, fe0a286) |
| `CORE-46` Opacity and acrylic/Mica background | S `opacity`, `vibrancy`, `hacks.enableFluentBackground` | DWM system backdrop on Windows 11, opaque fallback elsewhere. Text contrast is preserved. | planned | Step 20 | — |
| `CORE-47` Window frame style | S `frame` (native or thin) | Native title bar or a compact frame, DPI-aware (KIM-NFR-012) | planned | Step 20 | — |
| `CORE-48` Interface density | S `spaciness` | Compact and regular density tokens | planned | Step 20 | — |
| `CORE-49` Reduced animations | S `accessibility.animations` | Follows the Windows reduced-motion setting and has its own switch (KIM-NFR-012) | planned | Step 20 | — |
| `CORE-50` Background image | R plugin: "background - change Tabby background image and more..." | Excluded: it hurts legibility and the restrained Kim look (KIM-NFR-012). Revisit at Step 32. | excluded | — | — |
| `CORE-51` Custom CSS | S `appearance.css` | Excluded: there is no web renderer. Styling goes through theme tokens and scheme files. | excluded | — | — |

### 1.6 Keyboard and shortcuts

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `CORE-52` Fully configurable shortcuts | R: "Fully configurable shortcuts and multi-chord shortcuts" | Every GUI shortcut can be rebound in the config and the settings UI, and conflicts are reported (SUBAK-FR-034) | planned | Step 17 | — |
| `CORE-53` Multi-chord shortcuts | R: "…and multi-chord shortcuts"; S `multiHotkeyInput` | The `Ctrl+B` prefix table is a chord (SUBAK-FR-033), and users can define their own key sequences | planned | Step 17 | — |
| `CORE-54` Several bindings per action | S hotkey lists, e.g. `paste: ['Ctrl-Shift-V', 'Shift-Insert']` | The config uses the same list model | planned | Step 17 | — |
| `CORE-55` Free a shadowed key for the program | K unbind the hotkey | Explicit pass-through, e.g. `Ctrl+B Ctrl+B` and a send-next-key action (SUBAK-FR-034) | planned | Step 17 | — |
| `CORE-56` Smart Ctrl+C | S `ctrl-c` | Copies if text is selected, otherwise sends `^C`. Optional. | planned | Step 19 | — |
| `CORE-57` Line-editing hotkeys | S `home`, `end`, `previous-word`, `next-word`, `delete-previous-word`, `delete-line` | Mapped to the sequences each shell understands, per profile | planned | Step 17 | — |
| `CORE-58` Insert a new line without running | S `insert-new-line` (`Alt-Shift-Enter`) | Sends a literal newline, useful for multi-line prompts such as Claude Code | planned | Step 17 | — |
| `CORE-59` Scroll hotkeys, scroll on input | S `scroll-to-top`, `scroll-page-up`, `scrollOnInput` | Scrolls the view over the Host's scrollback, and jumps to the bottom on input | planned | Step 17 | — |
| `CORE-60` Clear terminal | S `clear` | Clears the screen and the pane's Host scrollback, on request | planned | Step 17 | — |
| `CORE-61` Alt as Meta | S `altIsMeta`, `switch-meta-option` | A per-profile setting with a toggle action | planned | Step 20 | — |
| `CORE-62` Backspace mode | S input `backspace` (`ctrl-h`, `ctrl-?`, `delete`) | Per-profile setting, mainly for SSH and serial | planned | Step 20 | — |
| `CORE-63` Command palette | S `command-selector` | Fuzzy palette over all actions, profiles and Terminals | planned | Step 33 | — |

### 1.7 Search, selection, clipboard, paste and links

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `CORE-64` Search in output | S `search` (`Ctrl-Shift-F`) | Searches the Host's scrollback, including output produced while no GUI was attached | planned | Step 19 | — |
| `CORE-65` Regex, whole-word, case-sensitive search | S `searchOptions` (`regex`, `wholeWord`, `caseSensitive`), `searchRegexAlwaysEnabled` | The same options. The CLI can also search captured text (SUBAK-FR-012). | planned | Step 19 | — |
| `CORE-66` Copy and paste shortcuts | S `copy` (`Ctrl-Shift-C`), `paste` (`Ctrl-Shift-V`, `Shift-Insert`), `select-all` | The same Windows defaults, rebindable. *Partial:* the Step 16 window copies with `Ctrl+Shift+C`/`Ctrl+Insert` and pastes (sanitized, bracketed when asked) with `Ctrl+Shift+V`/`Shift+Insert`; select-all, rebinding (Step 17) and the paste warning (Step 19) are not done | partial | Step 19 | T: `crates/subak/tests/gui_window.rs::keyboard_paste_copy_scroll_ime_and_focus` (Step 16, commit f660788) |
| `CORE-67` Copy on select | R: "Optional RMB paste and copy-on select (PuTTY style)"; S `copyOnSelect` (on by default on Windows) | Optional. The default is chosen in Step 19. | planned | Step 19 | — |
| `CORE-68` Right-click and middle-click actions | R: "Optional RMB paste…"; S `rightClick` (menu, paste, clipboard), `pasteOnMiddleClick` | The same choices. *Partial:* the Step 16 window does Windows Terminal's default (right click copies a selection, else pastes); the choices and middle-click paste are Step 19 | partial | Step 19 | I: `crates/subak/src/gui/window.rs` (`on_mouse`); M: SUBAK_GUI §9 step 5, pending (Step 16) |
| `CORE-69` Rich clipboard (copy as HTML) | S `copyAsHTML: true` | Puts plain text plus HTML (colors, font) on the Windows clipboard | planned | Step 19 | — |
| `CORE-70` Word separators for selection | S `wordSeparator` | Configurable, Unicode-aware double-click word boundaries. *Partial:* word selection with Windows Terminal's default delimiters, Korean as word characters; making them configurable is Step 19 | partial | Step 19 | T: `crates/subak-term/tests/view.rs::word_and_block_selection` (Step 15, commits db12228, c26b026, fe0a286) |
| `CORE-71` Bracketed paste | R: "Bracketed paste"; S `bracketedPaste: true` | Honors DECSET 2004. On by default. *Partial:* the view wraps pastes in 200~/201~ when the program asks and strips control characters so a paste cannot end the bracket (SUBAK-SEC-013); 2004 passes the inbox ConPTY; the GUI paste action is Step 19 | partial | Step 19 | T: `crates/subak-term/tests/view.rs::paste_is_sanitized_and_bracketed_when_asked` (Step 15, commits db12228, c26b026, fe0a286) |
| `CORE-72` Multiline paste warning | R: "multiline paste warnings"; S `warnOnMultilinePaste` | Shows a preview before sending. On by default. CLI input never prompts (KIM-FR-012). | planned | Step 19 | — |
| `CORE-73` Paste clean-up | S `trimWhitespaceOnPaste`, `replaceNewlinesWithSpacesOnPaste` | The same options | planned | Step 19 | — |
| `CORE-74` Clickable links | S package `tabby-linkifier` (URL, IP, Unix and Windows path handlers) | URLs, OSC 8 hyperlinks and file paths. Opens only on an explicit Ctrl+click, and shows the target first. *Partial:* OSC 8 links are modelled, kept in the history, drawn with a dotted underline and hit-tested by the view, which never opens them; URL/path detection and the Ctrl+click action are Step 19 | partial | Step 19 | T: `crates/subak-term/tests/conformance.rs::osc8_hyperlinks`, T: `crates/subak-term/tests/view.rs::focus_reports_and_hyperlink_hit_test` (Step 15, commits db12228, c26b026, fe0a286) |
| `CORE-75` Drop files to paste paths | S `pathDrop.ts` | Dropping from Explorer pastes the paths, quoted for the pane's shell | planned | Step 19 | — |
| `CORE-76` Save output to a file | R plugin: "save-output - record terminal output into a file" | Built in: `subak` capture (text or JSON) and an optional per-pane log file, off by default | planned | Step 19 | — |
| `CORE-77` Search selected text in the browser | R plugin: "search-in-browser - opens default system browser with a text selected…" | A context-menu action that opens the system browser only when clicked | planned | Step 19 | — |

### 1.8 Progress, notifications and activity

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `CORE-78` Progress detection | R: "Progress detection"; S `detectProgress: true` | OSC 9;4 progress and percentage heuristics, shown on the tab and the Windows taskbar | planned | Step 36 | — |
| `CORE-79` Notification on process completion | R: "Notification on process completion" | A Windows toast when a command finishes in an unfocused pane, using shell-integration marks. Local only. Complements the Claude `done` state (SUBAK-FR-050). | planned | Step 36 | — |
| `CORE-80` Activity indicator on background tabs | K activity marker on inactive tabs | tmux-style activity and silence monitoring per pane, shown on the tab, the session manager and the floating icon | planned | Step 36 | — |
| `CORE-81` Bell | S `bell` (off, visual, audible), `bellFlashFrame` | The same modes, plus a taskbar flash | planned | Step 36 | — |

### 1.9 Quake dropdown and global hotkey

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `CORE-82` Quake-style dropdown window | R: "Optional dockable window with a global spawn hotkey ("Quake console")"; S `appearance.dock`, `dockScreen`, `dockFill`, `dockHideOnBlur`, `dockAlwaysOnTop` | A GUI client docked to a screen edge. It is just another client, so hiding it never touches processes. | planned | Step 18 | — |
| `CORE-83` Global show/hide hotkey | S `toggle-window` (`Ctrl-Space`), `hacks.globalHotkey` | Uses `RegisterHotKey`, opt-in. No default that collides with Chinese IME toggling (`Ctrl+Space`). | planned | Step 18 | — |

## 2. Shells and profiles on Windows

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `SHELL-01` Windows PowerShell 5.1 | R: "PowerShell (and PS Core), WSL, Git-Bash, Cygwin, MSYS2, Cmder and CMD support"; S `windowsStock.ts` | A built-in, detected profile | planned | Step 20 | — |
| `SHELL-02` PowerShell 7 (pwsh) | R: "(and PS Core)"; S `powershellCore.ts` | Detected from PATH, Program Files and Store/winget installs | planned | Step 20 | — |
| `SHELL-03` CMD | R: "…and CMD support"; S profile "CMD (stock)" | A built-in profile | planned | Step 20 | — |
| `SHELL-04` CMD with Clink | R: "Proper shell experience on Windows including tab completion (via Clink)"; S default profile `local:cmd-clink` | Offered when the user has installed Clink. Starts `cmd.exe` with the user's own Clink. | planned | Step 33 | — |
| `SHELL-05` Bundled Clink | S `extras/clink` (`clink_<arch>.exe`) | Excluded: injects a bundled third-party DLL into `cmd.exe` and adds an update surface Subak does not control ([SECURITY.md](../SECURITY.md)). Use `SHELL-04` instead. | excluded | — | — |
| `SHELL-06` WSL distributions | R: "WSL"; S `wsl.ts`, `wslPath.ts` | One profile per installed distro. The Windows cwd is translated to the WSL path. | planned | Step 33 | — |
| `SHELL-07` Git Bash | R: "Git-Bash"; S `gitBash.ts` | Detected from the Git for Windows install | planned | Step 33 | — |
| `SHELL-08` Cygwin (32/64-bit) | R: "Cygwin"; S `cygwin32.ts`, `cygwin64.ts` | A detected profile | planned | Step 33 | — |
| `SHELL-09` MSYS2 | R: "MSYS2"; S `msys2.ts` | A detected profile per environment (MSYS, MINGW64, UCRT64) | planned | Step 33 | — |
| `SHELL-10` Cmder | R: "Cmder"; S `cmder.ts` | A detected profile using the user's Cmder install | planned | Step 33 | — |
| `SHELL-11` Visual Studio Developer Prompt | S `vs.ts` ("Developer Prompt for VS <version>") | Detected with `vswhere`, one profile per Visual Studio install | planned | Step 33 | — |
| `SHELL-12` Custom shell profiles | R: "Custom shell profiles" | TOML profiles with command, args, cwd, env, title, icon and color scheme (SUBAK-FR-011) | planned | Step 20 | — |
| `SHELL-13` Default profile | S `terminal.profile` | The user picks the default for `Ctrl+T` and the CLI | planned | Step 20 | — |
| `SHELL-14` Defaults per profile type | S `profileDefaults` | Defaults inherited per kind (local, SSH, serial) | planned | Step 20 | — |
| `SHELL-15` Per-profile environment | S `terminal.environment`, `windowsRefreshEnvironment`, `setComSpec` | Non-secret variables are saved. Secret ones become Credential Manager references (SUBAK-SEC-011). The environment is refreshed from the registry at spawn. | planned | Steps 20, 34 | — |
| `SHELL-16` Profile groups and tree | S `groups`, `showProfileTree`, `editProfileGroupModal` | Groups in the config, shown as a collapsible tree in the launcher and settings | planned | Step 33 | — |
| `SHELL-17` Quick launcher with recent profiles | S `profile-selector`, `showRecentProfiles: 3`, `showBuiltinProfiles`, `profileBlacklist` | Launcher with search, recent profiles and an option to hide built-in ones | planned | Step 33 | — |
| `SHELL-18` Hotkeys for profiles and groups | S `hotkeys.profile`, `profile-selectors`, `group-selectors` | A key can launch a profile or open a group | planned | Step 33 | — |
| `SHELL-19` Switch profile in place | S `switch-profile` | Replaces the pane's process with another profile, with confirmation when busy | planned | Step 33 | — |
| `SHELL-20` Shell integration (cwd, command marks) | S profile `rememberCwd`; K OSC-based cwd tracking | Opt-in snippets for PowerShell, bash, zsh and fish (OSC 7, OSC 9;9, OSC 133). User profiles are never edited without consent. | planned | Step 33 | — |
| `SHELL-21` Copy current path | S `copy-current-path` | Uses the cwd that shell integration reports | planned | Step 33 | — |
| `SHELL-22` Run as administrator | S package `tabby-uac` | Blocked: bridging an elevated pane into the non-elevated per-user Host crosses an elevation boundary, so it needs a security ADR first. Never silent (KIM-SEC-004). | blocked | Step 33 (after ADR) | — |
| `SHELL-23` Explorer context menu | S `shellIntegration.service.ts` (HKCU "Open Tabby here") | Opt-in "Open Subak here" verb, per user (HKCU) and without elevation. Removed on uninstall. | planned | Step 33 | — |
| `SHELL-24` Portable mode | R: "Tabby will run as a portable app on Windows, if you create a `data` folder…"; N asset `portable-x64.zip` | A `data\` folder next to the binaries holds config, state and logs. No registry writes. The Host pipe stays per user (SUBAK-SEC-010). | planned | Step 20 | — |
| `SHELL-25` Legacy WinPTY backend | S `useConPTY` switch | Excluded: ConPTY only (SUBAK-FR-016). It is always present on Windows 10 1809+ (KIM-NFR-010). | excluded | — | — |
| `SHELL-26` Docker containers | R plugin: "docker - connect to Docker containers" | A profile template that runs `docker exec -it`. A container picker could be an independent Kim tool. | planned | Step 33 | — |
| `SHELL-27` Open a tab on start | S `terminal.autoOpen` | The GUI attaches to existing Terminals and creates one only when the Host has none | planned | Step 17 | — |

## 3. SSH

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `SSH-01` SSH2 client | R: "Integrated SSH and Telnet client and connection manager"; R: "SSH2 client with a connection manager" | SSH sessions are owned by the Host, so they survive GUI close. An ADR chooses between a Rust library and `ssh.exe` in ConPTY. | planned | Step 34 | — |
| `SSH-02` Connection manager | R: "…with a connection manager" | Saved SSH profiles with groups, search and recents. The same objects are available from the CLI (KIM-FR-013). | planned | Step 34 | — |
| `SSH-03` Quick connect | S `defaultQuickConnectProvider: "ssh"`, CLI `quickConnect` | `user@host:port` in the launcher and the CLI | planned | Step 34 | — |
| `SSH-04` Password authentication | S `auth: 'password'` | The password is kept in Windows Credential Manager (DPAPI), never in config, logs or JSON (KIM-SEC-013) | planned | Step 34 | — |
| `SSH-05` Public-key authentication, including PuTTY keys | S `privateKeys`; N "fix(ssh): pass PuTTY (.ppk) private keys to WinSCP without converting them" | OpenSSH, PEM and `.ppk` keys. Key passphrases go in Credential Manager. | planned | Step 34 | — |
| `SSH-06` Private key discovery | S `AutoPrivateKeyLocator` | Offers the `%USERPROFILE%\.ssh\id_*` keys | planned | Step 34 | — |
| `SSH-07` Keyboard-interactive / 2FA | S `auth: 'keyboardInteractive'`, `keyboardInteractiveAuthPanel` | Prompts appear in the GUI. A non-interactive CLI call fails with a structured error (KIM-FR-011). | planned | Step 34 | — |
| `SSH-08` Windows OpenSSH agent | R: "Agent forwarding (incl. Pageant and Windows native OpenSSH Agent)"; S `ssh.agentType: 'auto'`, `agentPath` | Authenticates through `\\.\pipe\openssh-ssh-agent` | planned | Step 34 | — |
| `SSH-09` Pageant | R: "…incl. Pageant…" | Authenticates through Pageant | planned | Step 34 | — |
| `SSH-10` Host key verification, known hosts | S `ssh.verifyHostKeys: true`, `knownHosts`, `hostKeyPromptModal` | On by default, using the OpenSSH `known_hosts` format. A changed key blocks the connection. | planned | Step 34 | — |
| `SSH-11` Algorithm selection | S profile `algorithms` (kex, cipher, hmac, host key, compression) | Modern defaults. Legacy algorithms are opt-in per profile, with a warning. | planned | Step 34 | — |
| `SSH-12` Keep-alive and timeouts | S `keepaliveInterval: 5000`, `keepaliveCountMax: 10`, `readyTimeout` | The same settings | planned | Step 34 | — |
| `SSH-13` Reconnect and disconnect | S `restart-ssh-session`, `reconnect-tab`, `disconnect-tab` | Explicit reconnect, plus optional auto-reconnect with backoff. A dropped connection is always shown. | planned | Step 34 | — |
| `SSH-14` Banner and service messages | S `skipBanner`, `clearServiceMessagesOnConnect` | The same options | planned | Step 34 | — |
| `SSH-15` TERM and backspace per profile | S `term: 'xterm-256color'`, `input.backspace` | The same options | planned | Step 34 | — |
| `SSH-16` Warn on close | S `ssh.warnOnClose` | Uses the busy-close confirmation (SUBAK-FR-014) | planned | Step 34 | — |
| `SSH-17` Jump hosts | R: "Automatic jump host management"; S `jumpHost` | Chained jump profiles (ProxyJump), each with its own credentials | planned | Step 35 | — |
| `SSH-18` Proxy command, SOCKS and HTTP proxy | S `proxyCommand`, `socksProxyHost`, `httpProxyHost` | The same options | planned | Step 35 | — |
| `SSH-19` Local port forwarding | R: "X11 and port forwarding"; S `forwardedPorts` | Binds 127.0.0.1 by default. Listed in the GUI and CLI. | planned | Step 35 | — |
| `SSH-20` Remote port forwarding | R: "X11 and port forwarding"; S `PortForwardType` | Listed in the GUI and CLI | planned | Step 35 | — |
| `SSH-21` Dynamic (SOCKS) forwarding | S `PortForwardType` | A SOCKS listener bound to loopback | planned | Step 35 | — |
| `SSH-22` Manage forwards on a live session | S `sshPortForwardingModal` | Add or remove forwards without reconnecting | planned | Step 35 | — |
| `SSH-23` Agent forwarding | R: "Agent forwarding…"; S `agentForward: false` | Off by default. Enabled per profile, with a warning. | planned | Step 35 | — |
| `SSH-24` X11 forwarding | R: "X11 and port forwarding"; S `x11: false`, `ssh.x11Display` | Off by default. Needs an X server the user installs. | planned | Step 35 | — |
| `SSH-25` Login scripts | R: "Login scripts"; S `scripts`, `loginScriptProcessing.ts` | Expect/send rules. Secrets are referenced from Credential Manager and never written in the script. | planned | Step 35 | — |
| `SSH-26` Connection reuse | S `reuseSession: true`, `sshMultiplexer.service.ts` | New panes to the same host share one authenticated connection | planned | Step 35 | — |
| `SSH-27` Remember remote cwd | S `rememberCwd`, `cwd` | A new pane opens in the remote cwd that shell integration reports | planned | Step 35 | — |
| `SSH-28` Open in WinSCP | S `launch-winscp`, `ssh.winSCPPath` | Hands the session to a WinSCP the user installed, without putting passwords on the command line | planned | Step 35 | — |
| `SSH-29` Import OpenSSH config | S `OpenSSHImporter`, `StaticFileImporter` | Read-only import of `~/.ssh/config` hosts (HostName, User, Port, IdentityFile, ProxyJump) | planned | Step 36 | — |
| `SSH-30` Key names instead of paths for synced profiles | R plugin: "ssh-keymap - lets your synced Tabby SSH profiles reference private keys by name…" | Excluded: it exists to make cloud-synced profiles portable. Subak has no sync (`CFG-04`), and key paths stay local. | excluded | — | — |
| `SSH-31` In-app web authentication pop-ups | R plugin: "web-auth-handler - In-app web authentication popups" | Excluded: no embedded browser (Kim philosophy §5). Auth URLs open in the system browser through `CORE-74`. | excluded | — | — |

## 4. File transfer

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `XFER-01` SFTP browser | R: "SSH, SFTP and Telnet client available as a web app"; S `sftpPanel.component`, `open-sftp` | A native file panel that uses the session's existing SSH connection | planned | Step 35 | — |
| `XFER-02` Upload and download with progress | S `transfersMenu.component`; S SFTP actions "Download", "Download directory" | A transfer queue with progress, cancel and retry, with CLI equivalents (KIM-FR-013) | planned | Step 35 | — |
| `XFER-03` Create folder, delete, copy path | S `sftpContextMenu.ts` ("Create directory", "Delete", "Copy full path") | Deleting always asks for confirmation | planned | Step 35 | — |
| `XFER-04` Edit remote file locally | S electron `sftpContextMenu.ts` ("Edit locally") | Downloads the file to a per-user temp folder and uploads it on save, after confirmation | planned | Step 35 | — |
| `XFER-05` Drag and drop with Explorer | K | Drag files into and out of the panel | planned | Step 35 | — |
| `XFER-06` SFTP in its own tab | R plugin: "sftp-tab - open sftp tab for ssh connection like SecureCRT" | The panel can be docked or opened as a tab | planned | Step 35 | — |
| `XFER-07` Zmodem | R: "Direct file transfer from/to SSH sessions via Zmodem"; S `features/zmodem.ts` | The Host detects `rz`/`sz` in the pane stream, and the GUI picks the files. Works in local and serial panes too. | planned | Step 35 | — |

## 5. Serial

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `SER-01` Saved serial connections | R: "Integrated serial terminal"; R: "Saved connections" | Serial profiles. The Host keeps the session and its COM port open across GUI close. | planned | Step 36 | — |
| `SER-02` Port discovery | K port list in the profile editor | Lists COM ports with friendly device names | planned | Step 36 | — |
| `SER-03` Line settings | S `baudrate`, `databits`, `stopbits`, `parity` | The same options | planned | Step 36 | — |
| `SER-04` Flow control | S `rtscts`, `xon`, `xoff`, `xany` | The same options | planned | Step 36 | — |
| `SER-05` Slow send | S `slowSend` | Paces bytes for slow devices | planned | Step 36 | — |
| `SER-06` Readline input | R: "Readline input support"; S input mode `readline` | Edit a line locally, then send it on Enter | planned | Step 36 | — |
| `SER-07` Local echo | S input mode `local-echo` | The same option | planned | Step 36 | — |
| `SER-08` Hex input | R: "Optional hex byte-by-byte input…"; S input mode `readline-hex` | The same option | planned | Step 36 | — |
| `SER-09` Hexdump output | R: "…and hexdump output"; S output mode `hex` | The same option, switchable at runtime | planned | Step 36 | — |
| `SER-10` Newline conversion | R: "Newline conversion"; S `inputNewlines`, `outputNewlines` | Separate CR/LF/CRLF conversion for input and output | planned | Step 36 | — |
| `SER-11` Automatic reconnection | R: "Automatic reconnection" | Reopens the port when the USB device comes back | planned | Step 36 | — |
| `SER-12` Serial login scripts | S `SerialProfileOptions` extends `LoginScriptsOptions` | The same rules as `SSH-25` | planned | Step 36 | — |
| `SER-13` Timestamps on output | R plugin: "serial-timestamp - adds timestamps to serial terminal sessions" | Optional per-line timestamps, in the view and in saved logs | planned | Step 36 | — |
| `SER-14` Serial hotkeys | S `serial` (`Alt-K`), `restart-serial-session` | Rebindable open and restart actions | planned | Step 36 | — |

## 6. Telnet

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `TEL-01` Telnet client | R: "Integrated SSH and Telnet client and connection manager"; S package `tabby-telnet` | **Disabled by default.** Enabling it is an explicit setting, and Telnet profiles are visibly marked unencrypted. | planned | Step 36 | — |
| `TEL-02` Option negotiation | S `TelnetOptions` (terminal type, window size, suppress go-ahead) | The same options | planned | Step 36 | — |
| `TEL-03` Stream modes | S `TelnetProfileOptions` extends `StreamProcessingOptions` | Local echo, readline, hex and newline conversion, as for serial | planned | Step 36 | — |
| `TEL-04` Telnet login scripts | S `TelnetProfileOptions` extends `LoginScriptsOptions` | The same rules as `SSH-25`, plus a warning that secrets cross the network in clear text | planned | Step 36 | — |

## 7. Secrets

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `VAULT-01` Encrypted container for SSH secrets | R: "Integrated encrypted container for SSH secrets and configuration"; S `vault.service.ts` (AES-256-CBC, PBKDF2 passphrase) | Windows Credential Manager entries (DPAPI, bound to the Windows user) instead of a vault file. Config and state hold references only ([ADR 0006](adr/0006-state-and-configuration-storage.md)). | planned | Step 34 | — |
| `VAULT-02` OS keychain when the vault is off | S `passwordStorage.service.ts` (keytar) | Credential Manager is the only secret store | planned | Step 34 | — |
| `VAULT-03` Vault master passphrase | S `setVaultPassphraseModal`, `unlockVaultModal` | Excluded: protection follows the Windows logon (DPAPI), so there is no extra passphrase file to lose or brute-force. Re-authentication before revealing a secret is `VAULT-05`. | excluded | — | — |
| `VAULT-04` Encrypted configuration | S config keys `encrypted`, `vault` | Excluded: the config contains no secrets (KIM-SEC-013) and stays human-readable (SUBAK-FR-046) | excluded | — | — |
| `VAULT-05` Reveal a stored secret | S `showSecretModal` | An explicit action behind a Windows credential prompt or Windows Hello. Never logged or printed in JSON (KIM-SEC-013). | planned | Step 34 | — |
| `VAULT-06` Store key files | S vault secret type `file` | Prefer the SSH agent. Otherwise use a DPAPI-encrypted per-user blob, because Credential Manager blobs are size-limited. | planned | Step 34 | — |
| `VAULT-07` Plugin secrets in the vault | N "feat(settings): label a plugin's vault secret by its key's description" | Excluded: there are no plugins. Each Kim tool uses its own Credential Manager namespace. | excluded | — | — |
| `VAULT-08` Auto-typed sudo password | S package `tabby-auto-sudo-password` | Excluded: typing a stored password whenever output matches a prompt lets any remote program fake the prompt and collect it | excluded | — | — |

## 8. Configuration and settings sync

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `CFG-01` Settings UI | S package `tabby-settings` | A native settings view that writes the TOML config. Every setting is also available from the CLI (KIM-FR-013). | planned | Step 20 | — |
| `CFG-02` Human-readable config file | K `config.yaml` in `%APPDATA%\tabby` | `config.toml`. Unknown keys are preserved, and an invalid value falls back to its default with a precise error (SUBAK-FR-046). | planned | Step 20 | — |
| `CFG-03` Config schema version | S `version: 1` | A versioned schema with migrations (KIM-NFR-014) | planned | Step 20 | — |
| `CFG-04` Cloud config sync | S `configSyncSettingsTab`; K sync through a Tabby Web account | Excluded: no accounts and no cloud service (KIM-SEC-010) | excluded | — | — |
| `CFG-05` Sync to Gist or Gitee | R plugin: "sync-config - sync the config to Gist or Gitee" | Excluded: relies on a third-party cloud. Users can keep `config.toml` in a folder they sync themselves (local-only). | excluded | — | — |
| `CFG-06` Local export and import of settings | K open or copy the config from Settings | Export and import from the CLI, without secrets, with a merge preview | planned | Step 36 | — |
| `CFG-07` Import from Tabby | K Tabby `config.yaml` (profiles, color schemes, hotkeys) | A one-shot, read-only importer that reports what it skipped | planned | Step 36 | — |
| `CFG-08` Workspace profiles | R plugin: "workspace-manager - allows creating custom workspace profiles…" | Named layouts (Terminals, panes, profiles, cwd) that launch together, with an explicit launch command per pane | planned | Step 36 | — |

## 9. Plugins and themes ecosystem

All other plugins in the README are mapped to their own rows. docker is `SHELL-26`, title-control
`CORE-23`, save-output `CORE-76`, sync-config `CFG-05`, workspace-manager `CFG-08`,
search-in-browser `CORE-77`, sftp-tab `XFER-06`, background `CORE-50`, web-auth-handler `SSH-31`,
ssh-keymap `SSH-30` and serial-timestamp `SER-13`.

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `PLUG-01` Plugin manager | R: "Plugins and themes can be installed directly from the Settings view inside Tabby"; S `tabby-plugin-manager` | Excluded: there is no npm runtime inside Subak. Extra capabilities ship as independent Kim tools, installed only by explicit action (KIM-FR-001, KIM-FR-020, KIM-SEC-001). | excluded | — | — |
| `PLUG-02` Plugin API | R: "See HACKING.md and API docs…a very brief plugin development tutorial" | Excluded: replaced by the documented CLI/JSON contract and the local IPC (KIM-FR-010, SUBAK-SEC-012) | excluded | — | — |
| `PLUG-03` Plugin, command and provider blocklists | S `pluginBlacklist`, `commandBlacklist`, `providerBlacklist` | Excluded: there are no plugins to block (`PLUG-01`). Built-in features have their own switches (e.g. Telnet). | excluded | — | — |
| `PLUG-04` Installable theme packages | R Themes: hype, relaxed, gruvbox, windows10, altair, catppuccin, noctis | Excluded: themes as code packages need the plugin runtime (`PLUG-01`). Equivalent looks ship as data (`PLUG-05`). | excluded | — | — |
| `PLUG-05` Theme and color-scheme files | R: "Theming and color schemes"; S `customColorSchemes` | Plain data files (no code) in the config folder, plus import from common scheme formats | planned | Step 20 | — |
| `PLUG-06` MCP server for AI assistants | R plugin: "mcp-server - …Model Context Protocol server integration for Tabby…" | Subak's own opt-in local MCP server (SUBAK-FR-062, KIM-FR-021) | planned | Step 37 | — |
| `PLUG-07` Quick commands to one or all tabs | R plugin: "quick-cmds - quickly send commands to one or all terminal tabs" | Saved snippets sent as input to one or many Terminals, also from the CLI | planned | Step 33 | — |
| `PLUG-08` Keyword highlighting | R plugin: "highlight - Tabby terminal keyword highlight plugin" | Regex highlight rules in the config, drawn by the GUI | planned | Step 33 | — |
| `PLUG-09` Example plugin | R plugin: "clippy - an example plugin which annoys you all the time" | Excluded: a demo of the plugin API (`PLUG-02`) | excluded | — | — |

## 10. Web app and self-hosting

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `WEB-01` Web app | R: "SSH, SFTP and Telnet client available as a web app" | Excluded: Subak is a local-first native app, with no browser client and no account (KIM-SEC-010) | excluded | — | — |
| `WEB-02` Self-hosted web server | R: "(also self-hosted)" | Excluded: it would need a network listener. Subak's IPC is local and same-user only (SUBAK-SEC-010). | excluded | — | — |
| `WEB-03` Web demo | S package `tabby-web-demo` | Excluded: it demonstrates the web app (`WEB-01`) | excluded | — | — |

## 11. Application, CLI and platforms

| Tabby feature | Tabby reference | Subak approach | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| `APP-01` Windows installer | N asset `tabby-1.0.238-setup-x64.exe` | Installed per user by the Kim manager after hash and signature checks (KIM-FR-005, KIM-SEC-003, KIM-SEC-004) | planned | M1 (Kim) | — |
| `APP-02` Automatic updates | S `enableAutomaticUpdates: true` | Excluded: nothing updates by itself. Updates happen only through an explicit Kim action (KIM-SEC-001, KIM-SEC-002). | excluded | — | — |
| `APP-03` Usage analytics | S `enableAnalytics: true` | Excluded: no tracking and no telemetry (KIM-SEC-011, KIM-SEC-012) | excluded | — | — |
| `APP-04` Command-line launcher | S CLI `open [directory]`, `run [command...]`, `profile [profileName]`, `paste [text]`, `recent [index]`, `quickConnect` | The headless `subak` CLI with `--json` (KIM-FR-010). It opens a Terminal in a directory, with a command or a profile, and can send text. | planned | Step 33 | — |
| `APP-05` Single instance | K a second launch hands its arguments to the running app | All clients talk to the one per-user Host (SUBAK-FR-001). The GUI focuses the window that is already open. | planned | Step 17 | — |
| `APP-06` Tray icon | S `hideTray` | A floating icon or a tray icon, both optional (SUBAK-FR-030) | planned | Step 18 | — |
| `APP-07` Start minimized or at login | S CLI `--hidden` ("Start minimized") | Starts minimized on request. The Host and the icon start at login only if the user enabled that (SUBAK-FR-002). | planned | Step 18 | — |
| `APP-08` Interface language | S `language`, `locale/`; R translated READMEs (incl. ko-KR) | English first, Korean planned. Translations are data files. | planned | Step 20 | — |
| `APP-09` Welcome tab and release notes | S `enableWelcomeTab`, `releaseNotesTab` | The first-run page explains detach vs. kill. Release notes come from the local Kim catalog, with no network call. | planned | Step 20 | — |
| `APP-10` Safe mode | S `safeModeModal` | Excluded: there are no plugins to disable, and a broken config already falls back to defaults (SUBAK-FR-046) | excluded | — | — |
| `APP-11` Electron flags and DevTools | S `electronFlags`; CLI `--debug` ("Show DevTools on start") | Excluded: there is no Electron. Diagnostics are local logs that contain no terminal content (KIM-SEC-012). | excluded | — | — |
| `APP-12` Debug state and output hotkeys | S `debug-save-state` to `debug-paste-output` | Excluded: developer tooling. Capture with JSON output covers inspection (SUBAK-FR-012). | excluded | — | — |
| `APP-13` Windows ARM64 build | N assets `setup-arm64.exe`, `portable-arm64.zip` | Blocked: no ARM64 test machine yet, and untested platforms are not claimed (Product requirements §2) | blocked | — | — |
| `APP-14` Linux packages | N assets AppImage, deb, rpm, pacman, tar.gz | Linux Host and clients come in M4 (SUBAK-FR-070, KIM-NFR-011) | planned | M4 | — |
| `APP-15` macOS app (Touch Bar, Dock menu) | N assets dmg, zip; S `touchbar.service.ts`, `dockMenu.service.ts` | Excluded: macOS is out of scope (Product requirements §2) | excluded | — | — |

## Security and privacy deviations

Subak deliberately differs from Tabby's defaults in these places. A row in the matrix may not
relax any of them without an ADR.

| Topic | Tabby (v1.0.238) | Subak | Requirements |
| --- | --- | --- | --- |
| **No plaintext secrets** | Secrets live in a passphrase-encrypted vault, or in the OS keychain (keytar) when the vault is off | Secrets go only to Windows Credential Manager or DPAPI. Config and state hold references. Secrets never appear in logs, JSON, crash reports, login scripts or state. | KIM-SEC-013, SUBAK-SEC-011 |
| **Auto-typed passwords** | `tabby-auto-sudo-password` is built in | Excluded (`VAULT-08`) | KIM-SEC-013 |
| **Telnet off by default** | Built-in connection type, available out of the box | Disabled until the user explicitly enables it. Profiles are marked unencrypted. | Kim philosophy §6 |
| **No hidden network calls** | `enableAnalytics: true` and `enableAutomaticUpdates: true` by default | No network traffic unless the user asked for it. No analytics, no automatic download, and update checks are display-only and off by default. | KIM-NFR-004, KIM-SEC-002, KIM-SEC-011, KIM-SEC-012 |
| **No cloud** | Config sync service, web app, self-hosted web server | None. No account is required for anything. | KIM-SEC-010, SUBAK-SEC-010 |
| **No in-process third-party code** | npm plugins run inside the app | No plugin runtime. Other capabilities come as independent, verified Kim tools. | KIM-FR-020, KIM-SEC-001, KIM-SEC-003 |
| **No bundled injector** | Ships Clink and injects it into `cmd.exe` | Uses a Clink the user installed (`SHELL-04`) | [SECURITY.md](../SECURITY.md) |
| **No silent elevation** | `tabby-uac` elevated tabs | Blocked pending an ADR (`SHELL-22`) | KIM-SEC-004 |
| **Local control only** | Optional web front end | Same-user local IPC. Remote control needs explicit enablement. | SUBAK-SEC-010, SUBAK-SEC-012 |
| **SSH defaults** | Host keys verified; agent and X11 forwarding off | The same, plus port forwards bind to loopback by default and legacy algorithms need opt-in with a warning | Kim philosophy §6 |
| **Links and paste** | Click-to-open links; bracketed paste and multiline warning on | Links open on Ctrl+click only and show the target first, with no embedded browser. Paste safety stays on by default, and CLI paths never prompt. | KIM-FR-012 |

## Core vs optional independent capability

Kim prefers small independent tools over a monolith (KIM-FR-001, KIM-FR-020). The table proposes
where each area belongs. The named Step confirms or changes the placement.

| Capability | Placement | Why |
| --- | --- | --- |
| Emulation, tabs, panes, persistence, search, clipboard, links, themes, fonts, local shell profiles, notifications | **Subak core** | Inseparable from the Host and the GUI. This is why Subak exists. |
| SSH sessions and credential handling | **Subak core** (Host-owned), Step 34 | Remote sessions must survive GUI close the same way local ones do, with one audited credential path |
| SFTP browser, Zmodem | Protocol handling in core. The SFTP browser UI could become an independent Kim file-transfer tool that uses Subak's connection through the CLI. Decided in Step 35. | Keeps the core small without duplicating authentication |
| Serial, Telnet | **Candidate independent Kim tool**: a small bridge (COM port or Telnet socket to stdio) that runs as an ordinary pane process, which the Host keeps alive like any shell. Decided in Step 36. | Keeps protocol code and its attack surface out of the core. Telnet stays off unless installed or enabled. |
| Tabby and OpenSSH config import | Optional: a one-shot import command or an independent tool | Runs once and has no runtime cost |
| MCP server | Optional and opt-in (SUBAK-FR-062), never the only interface (KIM-FR-021) | Agents get structured access, while humans lose nothing without it |
| Docker picker, quick commands, workspaces, highlighting | Profiles, snippets, layouts and rules are core data. Richer pickers can be independent Kim tools that call the Subak CLI. | Rich features without a plugin host |
| Color-scheme packs, translations | Data files | No code and no runtime |
| npm plugins, web app, cloud sync | Not built | See the excluded rows |

## How evidence is recorded

- **Evidence cell.** One or more entries, using the verification codes from
  [PRODUCT_REQUIREMENTS.md](PRODUCT_REQUIREMENTS.md): `T:` automated test (crate path and test
  name), `M:` manual check on real Windows (Windows version and build, date, and the run in
  [e2e/LEDGER.md](../e2e/LEDGER.md)), `I:` inspection, `D:` documentation, each with the PR or
  commit that introduced it. `—` means no evidence yet.
- **`implemented`** needs at least one `T:` entry, plus an `M:` entry when the behavior depends on
  the GUI or the OS. **`partial`** says in the approach cell what is missing. Neither status is
  allowed without evidence.
- **`excluded` and `blocked`** keep their rationale. A blocked row names what unblocks it. Moving
  a row between `excluded` and `planned` records the reason, and needs an ADR when it trades one
  principle against another (Kim philosophy §8).
- **The Step that implements a row updates it in the same PR.** Row IDs are permanent: a dropped
  row is marked *withdrawn*, not deleted, and a new Tabby feature gets the next free ID in its
  group.
- **Audits.** Step 32 (and any later audit) re-fetches the sources, updates the Reference line,
  adds or withdraws rows, confirms or corrects every **K** reference, recomputes the summary
  counts, and appends a line to the audit history.

## Summary counts

| Status | Rows |
| --- | --- |
| `planned` | 166 |
| `partial` | 14 |
| `implemented` | 0 |
| `excluded` | 26 |
| `blocked` | 3 (`CORE-06` Sixel, `SHELL-22` run as administrator, `APP-13` Windows ARM64) |
| **Total** | **209** |

| Group | planned | partial | excluded | blocked | Total |
| --- | --- | --- | --- | --- | --- |
| 1. Terminal core (`CORE`) | 66 | 14 | 2 | 1 | 83 |
| 2. Shells and profiles (`SHELL`) | 24 | 0 | 2 | 1 | 27 |
| 3. SSH (`SSH`) | 29 | 0 | 2 | 0 | 31 |
| 4. File transfer (`XFER`) | 7 | 0 | 0 | 0 | 7 |
| 5. Serial (`SER`) | 14 | 0 | 0 | 0 | 14 |
| 6. Telnet (`TEL`) | 4 | 0 | 0 | 0 | 4 |
| 7. Secrets (`VAULT`) | 4 | 0 | 4 | 0 | 8 |
| 8. Configuration and sync (`CFG`) | 6 | 0 | 2 | 0 | 8 |
| 9. Plugins and themes (`PLUG`) | 4 | 0 | 5 | 0 | 9 |
| 10. Web app (`WEB`) | 0 | 0 | 3 | 0 | 3 |
| 11. Application and platforms (`APP`) | 8 | 0 | 6 | 1 | 15 |

## Audit history

| Date | Tabby reference | Step | Change |
| --- | --- | --- | --- |
| 2026-11-03 | v1.0.238 (2026-10-07) | Step 03 | Initial inventory. Nothing implemented. |
| 2026-11-04 | v1.0.238 (2026-10-07) | Step 15 | Ten terminal-core rows `partial` (emulator, xterm extensions, Unicode, CPU renderer, bold as bright, fallback font, cursor shapes, word separators, bracketed paste, OSC 8 links); `CORE-06` stays blocked with the ConPTY decision of ADR 0036. No Tabby re-fetch. |
| 2026-11-04 | v1.0.238 (2026-10-07) | Step 16 | Four more terminal-core rows `partial` with the Subak window (`CORE-33` reopen the last Terminal, `CORE-44` zoom, `CORE-66` copy and paste shortcuts, `CORE-68` right click); `CORE-01`–`03`, `07` and `45` notes updated. No Tabby re-fetch. |
