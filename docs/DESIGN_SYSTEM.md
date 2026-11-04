# Kim Design System

Status: **current** · Step 04 · Applies to Kim Manager and every Subak GUI surface ·
Tokens: [design/tokens.json](../design/tokens.json) (checked by `cargo xtask design-check`) ·
Toolkit: [ADR 0014](adr/0014-gui-technology.md)

Kim looks like a quiet instrument: a nearly black surface, compact type, very little chrome, and
one controlled lilac accent that marks *where you are* and *what needs you*. Terminal content is
the hero; everything else steps back.

## 1. Principles

1. **Content first.** Chrome is small and dim; the terminal grid and the user's text get the
   contrast budget.
2. **One accent, used sparingly.** Lilac marks focus, selection, the active tab and primary
   actions — never decoration.
3. **Dense but clear.** A 4 px grid, compact controls (28 px), generous only where reading
   happens.
4. **State is never color-only.** Every status has a shape and a text label/tooltip.
5. **Calm motion.** Short (80–160 ms), decelerating, purposeful; none when the user asks for
   reduced motion.
6. **Keyboard is first-class.** Every action is reachable and visible-focused by keyboard.
7. **Shared language, separate runtimes.** Both GUIs consume the same tokens; no headless
   binary links GUI code ([ADR 0002](adr/0002-independent-tool-binaries.md)).

## 2. Tokens

The token file is the source of truth; this section explains intent. Sizes are logical pixels
(device-independent), multiplied by the monitor's DPI scale.

### 2.1 Color — theme `kim-dark` (default)

| Role | Token | Value | Use |
| --- | --- | --- | --- |
| Surface | `bg.base` | `#0E0E12` | Window and terminal background |
| | `bg.raised` | `#15151B` | Tab bar, side panels, list background |
| | `bg.overlay` | `#1C1B24` | Menus, dialogs, active tab, tooltips |
| | `bg.hover` / `bg.selected` | `#22212C` / `#2A2838` | Hover and selected rows/tabs |
| Lines | `border.subtle` | `#26252F` | Dividers, pane separators (decorative) |
| | `border.strong` | `#6A6680` | Input/control boundaries (≥ 3:1) |
| Text | `text.primary` / `.secondary` / `.muted` | `#E8E6F0` / `#A9A6B8` / `#9592A8` | Body / secondary / captions (all ≥ 4.5:1 on every surface) |
| | `text.disabled` | `#55536A` | Disabled only (exempt from contrast rules, never the only cue) |
| Accent | `accent` / `.hover` / `.pressed` | `#B7A6F5` / `#C7B9FA` / `#9B86F0` | Focus, selection, active tab marker, primary button |
| | `accent.soft` | `#2B2742` | Selected-row tint, accent chips |
| | `text.on-accent` | `#0E0E12` | Labels on accent fills (dark on lilac, 9:1) |
| Focus | `focus.ring` | `#C9BCFF` | 2 px ring, 1 px offset |
| Status | `status.success/warning/danger/info` | `#8BC79A` `#E8C27A` `#F0868F` `#8AB4F8` | Results, banners, toasts |

Measured contrast ratios are printed by `cargo xtask design-check`; the build fails if any
declared pair drops below its minimum (WCAG 2.2: 4.5:1 text, 3:1 non-text UI and focus).

### 2.2 Terminal palette

Foreground `#D8D6E0` on `#0E0E12` (13.4:1), cursor lilac, selection `#3A3460`. Search matches
(Step 19) are tinted `terminal.search-match` `#4D3F1C` (the foreground stays ≥ 7:1 on it); the
current match is dark text on `terminal.search-current` `#E8C27A` (≥ 7:1), so it differs from the
others in lightness, not hue alone, and the search bar says "3 of 17" in words. The 16 ANSI
colors are tuned for the near-black background; all foreground colors except ANSI black are
≥ 4.5:1, and bright black (used for comments/dim text) stays readable. Programs that set 24-bit
colors are rendered exactly; the palette only maps the 16 indexed colors.

### 2.3 Typography

| Token | Value |
| --- | --- |
| UI family | Segoe UI Variable Text → Segoe UI → **Malgun Gothic** (Korean) → Segoe UI Emoji → Segoe UI Symbol |
| Mono family | Cascadia Mono → Consolas → D2Coding (if installed) → **Malgun Gothic** → Segoe UI Emoji → Segoe UI Symbol |
| Sizes | caption 11 · body 12 · emphasis 13 · title 15 · terminal 13 (user-adjustable) |
| Weights | regular 400 · semibold 600 (titles, active tab) |
| Line height | UI 1.35 · terminal 1.2 (cell height = ceil(size × 1.2) at the device scale, or the font's ascent + descent when larger: 17 px for Cascadia Mono at 100 %, [TERMINAL_EMULATOR §5.4](TERMINAL_EMULATOR.md)) |

### 2.4 Space, radius, size

- Space scale `0, 2, 4, 6, 8, 12, 16, 24, 32`. Default gaps: 8 inside controls, 12 between
  groups, 16 dialog padding.
- Radius `4` (controls, chips), `6` (menus, tabs), `10` (dialogs, floating icon). No radius on
  terminal panes.
- Key sizes: tab bar 32, tab 96–220 wide, control 28 (compact 24), status dot 8, floating icon
  40, progress bar 4, minimum hit target 24×24.

### 2.5 Motion

| Token | Value | Used for |
| --- | --- | --- |
| `duration.fast` | 80 ms | hover, press |
| `duration.base` | 120 ms | tab switch highlight, focus ring appear, list selection |
| `duration.slow` | 160 ms | menu/dialog fade + 4 px rise |
| `easing.standard` | cubic-bezier(0.2, 0, 0, 1) | everything |
| `pulse.period` | 1600 ms | `in-progress` indicator (opacity 0.45 ↔ 1) |

Never animate terminal content, layout of panes, or anything during typing.

### 2.6 Elevation

Flat by default. Overlays (menus, dialogs, floating manager) use `bg.overlay`, a 1 px
`border.subtle` and one soft shadow (`0 8 24 rgba(0,0,0,0.45)`). No stacked shadows, blur or
glass effects (they cost GPU time on idle windows).

Noeul's floating pins (Step 54, [ADR 0044](adr/0044-noeul-pin-transforms-and-control.md)) use a
1 px `border.subtle` outline (`pin.border`) and a smaller soft shadow, `pin.shadow`
(`0 2 10 rgba(0,0,0,0.40)`), both switchable off. A pin window is hit-tested by its pixels, so its
shadow is also the band that resizes it; the overlay's 24-pixel blur would make a ring around
every pin catch clicks meant for the program below. The shadow is painted once into the pin's
retained surface, never animated.

## 3. Components

States for every interactive component: **rest · hover · pressed · focused (ring) · selected ·
disabled**. Focus ring is always drawn on keyboard focus, never on mouse click.

### 3.1 Status dots and the Claude indicator

8 px marks, always with a tooltip and an accessible name ("Claude: needs input").

| State | Color token | Shape (color-blind safe) | Motion |
| --- | --- | --- | --- |
| `idle` | `claude.idle` | filled circle, dim | none |
| `in-progress` | `claude.in-progress` | filled circle | pulse (static ring when reduced motion) |
| `need-input` | `claude.need-input` | filled circle + 1 px outer ring | none (attention without flashing) |
| `error` | `claude.error` | filled triangle | none |
| `done` | `claude.done` | filled circle with check notch | none; cleared when viewed |
| `unknown` | `claude.unknown` | hollow ring | none |
| `none` | — | nothing | — |

Tool install status uses the same shapes with `status.*` colors: installed (filled), update
available (ring + dot), incompatible (triangle), not installed (nothing).

### 3.2 Kim Catalog (Kim Manager)

- Two-pane layout: list (min 320) | details. Window min 720×480.
- **Row** (48 high): 32 px tool icon · name (emphasis) · one-line description (muted) · version
  (mono, muted) · status chip. Selected row: `accent.soft` fill + 2 px lilac left marker.
- **Details**: icon 64, name (title), description, version/channel/license/platform table,
  release notes (scrollable), and one explicit primary action (`Install` / `Update to x.y.z` /
  `Uninstall` as secondary-danger). No action is ever triggered by selection alone.
- Offline/catalog-stale banner at top (info), with "Refresh catalog" button — never automatic.
- Details also show (Step 47) the release stage (`Planned` / `Preview` / `Available`), support,
  how the tool runs, and below the action: features, keyboard shortcuts (keys in mono, scope
  in muted text, fixed keys marked), *What it may do on your computer* (each permission in
  emphasis with its detail, opt-in ones marked), command-line and MCP entry points (planned
  ones marked), documentation links as selectable text (no browser is started), install
  location and *Updates: manual only*. A planned tool shows why there is nothing to install
  instead of a button. The pane scrolls; the list row shows `planned` instead of a version.

### 3.3 Install progress

- Inline in the details pane: phase label (`Downloading 12.4 / 38.0 MB`, `Verifying
  signature`, `Installing`, `Done`, `Failed`), 4 px bar (lilac on `bg.selected`, indeterminate
  shimmer only when size unknown; static stripes in reduced motion), `Cancel` (secondary).
- Failure: danger banner with the stable error code, a human sentence, `Retry` and `Copy
  details` (no secrets). Old version state is stated explicitly ("Version 0.2.0 is still
  installed and working").

### 3.4 Subak floating icon and tray

- Floating icon: 40×40, radius 10, `bg.overlay`, 1 px `border.subtle`, Subak glyph centered;
  aggregate Claude indicator as a 10 px badge at the bottom-right. Draggable; snaps 8 px to
  monitor edges; hover shows a tooltip "Subak — 3 terminals, 1 needs input".
- Click: opens the compact manager (§3.6) anchored to the icon. Right-click: menu (Open
  Subak, New terminal, Hide icon, Quit GUI — never "kill").
- Tray: same aggregate badge on the 16/24/32 px tray icon; menu identical.

### 3.5 Tab bar and terminal window

- Integrated title bar: tab bar (32) in the title area, caption buttons at the right (46 wide,
  native hover colors; close button hover uses `status.danger`).
- **Tab**: 6 px top radius, 8 px horizontal padding, Claude dot (left) · name (ellipsis) ·
  close `×` on hover/active. Active tab: `bg.base` fill joining the terminal surface, semibold
  name, 2 px lilac top marker. Inactive: `bg.raised`, `text.secondary`.
- `+` button after the last tab (new Terminal), overflow chevron when tabs exceed width.
- Terminal surface: `terminal.background`, 8 px inner padding (configurable 0–16), no border.
- Status line (optional, tmux-style, Step 25): 22 px at the bottom, `bg.raised`, mono caption.

### 3.6 Session manager (full and compact)

- List of all Terminals the Host owns: indicator · name · state (`running`, `stopped`,
  `restorable`, `failed`) · windows/panes count · last activity. Search field on top (`/` or
  `Ctrl+F` focuses it).
- Actions: Open, Rename (`F2`), New (`Ctrl+N`), Stop, Delete (danger, confirm). "Close manager"
  never affects Terminals.

### 3.7 Panes

- Divider: 1 px `border.subtle`; drag handle area 6 px (invisible). Active pane: 1 px `accent`
  inner border at 60 % opacity (optional, default on when > 1 pane).
- Zoomed pane: small `Z` chip in the status line / tab.

### 3.8 Menus, dialogs, errors

- **Menu**: `bg.overlay`, radius 6, item height 26, shortcut hints right-aligned (muted mono),
  separators `border.subtle`. Opens in 160 ms (fade + 4 px rise).
- **Dialog**: max width 440, radius 10, title 15 semibold, body 12, buttons right-aligned;
  primary = accent fill + `text.on-accent`; destructive = `status.danger` outline/text; default
  focus is the **safe** choice (Cancel). Kill confirmation lists exactly what will end
  ("2 panes: `cargo run`, `claude`").
- **Errors**: inline banner (danger tint `#F0868F` at 12 % on `bg.raised`, danger icon, message,
  error code in mono, actions). Toasts (bottom-right, 4 s, pause on hover) only for
  non-critical outcomes; errors needing action stay until dismissed.
- **Empty / disconnected / loading** states: one muted sentence plus one action (e.g. "Host
  not running — Start"), never a blank window.

## 4. Interaction

### 4.1 Keyboard-only

- Tab order follows reading order; `F6` cycles regions (tab bar → terminal → status line /
  list → details); `Esc` closes menus/dialogs; arrow keys move within lists, tabs and menus;
  `Enter`/`Space` activate; `F2` renames; `Shift+F10`/`Menu` opens context menus.
- Terminal focus captures keys for the program; application shortcuts follow
  [ADR 0009](adr/0009-gui-clients-and-keyboard-dispatch.md) dispatch order (IME guard → prefix →
  app shortcuts → PTY).
- A keyboard help overlay (`Ctrl+B ?` and `F1`) lists live bindings.
- Every GUI's shortcut list, keyboard-only binding editor and "Keyboard shortcuts: on/off"
  switch follow [HOTKEY_CONTRACT §11](HOTKEY_CONTRACT.md); the keys above are its reserved
  navigation actions.

### 4.2 High-DPI and multiple monitors

- Per-monitor DPI awareness v2. All tokens are logical px × scale; hairlines (1 px dividers)
  stay one *physical* pixel; text and glyph atlases re-rasterize on DPI change (no bitmap
  scaling).
- Verified scales: 100, 125, 150, 175, 200 %. Moving a window between monitors re-lays out
  without a flash of wrong size.

### 4.3 Contrast and Windows High Contrast

- `kim-dark` meets the declared pairs in `tokens.json` (checked in CI).
- When Windows High Contrast / Contrast themes are active, GUIs switch to system colors
  (`COLOR_WINDOW`, `COLOR_WINDOWTEXT`, `COLOR_HIGHLIGHT`, …) and drop the lilac accent.

### 4.4 Korean and Unicode

- Fallback chains in §2.3; Hangul syllables and CJK are **wide** (2 cells) per Unicode East
  Asian Width; ambiguous-width characters default to narrow (configurable).
- IME: composition text is drawn inline at the cursor with an underline; no shortcut fires
  during composition (ADR 0009).
- Emoji render in color where the renderer supports it; otherwise monochrome fallback, never
  tofu boxes for supported fonts.

### 4.5 Reduced motion

Honors Windows "Show animations in Windows" (`SPI_GETCLIENTAREAANIMATION`) and the app setting:
all durations become 0, pulses become static rings, progress shimmer becomes static stripes.

## 5. Implementation notes

- Tokens are consumed by a future `kim-design` crate (created when the second GUI needs it,
  ADR 0002) that embeds `tokens.json` at build time; headless binaries never depend on it.
- Visual regression: GUIs render headlessly in tests (ADR 0014) and compare snapshots of key
  components at 100 % and 200 %.
- Mock-ups: `design/mockups/*.png` are produced from the tokens by the reproducible generator
  described in [design/README.md](../design/README.md).
