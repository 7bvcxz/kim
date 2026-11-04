# ADR 0048 — Subak search, clipboard and links: a headless bounded search over the view's model, questions before acting on terminal content, the outside world behind traits

Status: **Accepted** · Date: 2026-11-04 · Step 19 · Builds on [ADR 0030](0030-host-terminal-model-scrollback-and-replay.md)
(the Host's bounded model), [ADR 0036](0036-terminal-emulation-and-rendering-path.md) (one model
for the Host and the views, the CPU renderer), [ADR 0039](0039-subak-window-plain-win32-client.md)
and [ADR 0042](0042-subak-window-tabs-and-safe-close.md) (the window, its modal parts, keys from
the hotkey contract) · Details, user guide and manual checks: [SUBAK_GUI.md §12](../SUBAK_GUI.md)

## Context

Step 19 asks for fast search of a pane's retained scrollback with keyboard controls,
incremental matches, next/previous and a *safe* regular expression; selection by mouse and
keyboard, copy, paste, a confirmation for multi-line or suspicious pastes, native clipboard
integration and a plain-text paste; opening and copying detected links with validation; an
optional export of output only with deliberate consent and a warning about sensitive content;
high-volume history without loading it into GPU memory, within the bounded retention and the
IPC permissions of Step 13; and no dependency of the CLI, headless mode or the Host core on the
GUI's search components. On this machine nothing may touch the maintainer's real clipboard,
open a browser or send output anywhere.

What exists: the client view already holds the same model as the Host — screen and the whole
retained history (2 000 rows / 4 MiB by default, up to 100 000 rows), replayed in the attach
snapshot — and selection, copy, sanitized and bracketed paste, OSC 8 hit tests (Step 15); the
window copies and pastes only `CF_UNICODETEXT` and has a busy confirmation that is a one-row
modal banner with Cancel focused (Step 17); one secret detector redacts captures and commands
(Steps 13, 16).

## Decision

1. **Search is headless and runs over the view's own model** (`subak_term::search`). The
   window's view already carries everything the Host retains for the pane, so the search needs
   no new IPC method, sees output printed while no window was attached, and respects the
   Host's bounds and permissions by construction (only `cli`/`gui` clients ever get a snapshot,
   IPC_PROTOCOL §4). History lines are read as text straight from their compact VT form (no
   cell decoding), logical line by logical line (rows joined across soft wraps), newest first;
   only the matches are kept (cell spans), and the CPU renderer tints them like the selection —
   no GPU memory, no second copy of the output. *Rejected:* a Host-side `pane.search` (a second
   path to the same data, an IPC round trip per keystroke, and a CPU cost in the process that
   owns every pane); searching rendered frames.
2. **Bounded by construction: the `regex` crate**, with Ddalgak's feature set (ADR 0028;
   already in the lock). Its matching is linear in the text (no backtracking, so no
   catastrophic patterns); a pattern is at most 1 000 characters, its compiled program at most
   512 KiB, its lazy DFA 2 MiB and its nesting 64 deep — over a cap it is refused as "too
   complex", never run. A search stops after 10 000 matches (keeping the newest) or 250 ms
   (checked every 32 logical lines) and says which. Literal search is the default (the pattern
   escaped); case folding, whole word and regex are options. `Search::refresh` after output
   keeps the matches in history lines (which cannot change) and searches only the lines that
   entered the history since and the screen; the alternate screen is searched alone.
   *Rejected:* a hand-written engine (unproven), `fancy-regex`/PCRE (backtracking).
3. **The window's UI is modal like the rename field, drawn where banners are.** The search bar
   (`Ctrl+Shift+F`, action `search.open`) is the banner row over the top of the pane: the field
   takes characters and its editing keys, `Enter`/`F3`/`Up` go to the next older match and
   `Shift+Enter`/`Shift+F3`/`Down` to the next newer (wrapping), `Alt+C`/`Alt+R`/`Alt+W` toggle
   the options, `Esc` closes and keeps the current match selected; shortcut chords still fire;
   nothing reaches the program meanwhile. Revealing a match keeps it below the bar's row.
4. **Questions before acting on terminal content, Cancel focused.** One generic question
   (`gui::ask`) in the banner row, with the busy confirmation's rules (`Esc`, `Enter` on Cancel
   or a click on it does nothing; `Tab`/arrows move; keys meanwhile reach nothing):
   - **Paste** asks when it would run lines (a line break without bracketed paste), carries
     control characters (always removed — a sign of a disguised command) or is larger than
     5 KiB; it names what would happen and the first line, and offers "As one line" (line
     breaks become spaces, surrounding blanks trimmed). A paste is always **plain text**: only
     `CF_UNICODETEXT` is read; HTML, RTF, files and images are never interpreted.
   - **Links**: `Ctrl+click` (or the pane menu) on an OSC 8 hyperlink or an `http(s)://`
     address detected in the text. Every target is validated (`subak_term::links`): `http` and
     `https` only, no user name or password, no blanks, control characters or backslash in the
     authority, a host, at most 2 048 characters; the normalized target is what is shown and
     opened. A detected address (its text is its target) opens at once; a hyperlink (its text
     may differ from its target) shows the target first and opens only on "Open"; a refused one
     says why and offers "Copy link". Opening is `ShellExecuteW` with the verb `open` and the
     target as the file — never a command line, never a shell. (`SUBAK-SEC-013`.)
   - **Save output…** builds the view's history and screen as logical lines, runs Subak's one
     secret detector (`crate::redact`, the rules of `subak capture`) and asks, naming the
     count: "Save with them hidden…" (`***`, the first choice after Cancel) or "Save exactly…";
     then the common save dialog, and an atomic write (temporary file, rename) only to the
     chosen path. Nothing else is written, nothing is sent anywhere.
5. **The outside world behind traits** (`gui::clip`): `Clipboard` (read plain text; write text
   and optional CF_HTML), `Opener` (the open verb) and the save picker. The Win32 versions live
   in `window.rs`; the off-screen test window uses recording fakes (formats, verb and target,
   the suggested name), so no automated test touches the real clipboard, a browser or a dialog.
6. **Keyboard selection is mark mode** (`selection.mark`, `Ctrl+Shift+M`, as in Windows
   Terminal): the arrows move a one-cell caret from the cursor, Shift grows the selection,
   Ctrl moves by words (a growing selection ends at a word's last cell), Home/End, PageUp/Down
   and Ctrl+Home/End jump; Enter copies; Esc leaves. Shift+arrows outside mark mode stay the
   program's (PSReadLine uses them). `selection.all` is `Ctrl+Shift+A`; "Copy with colours"
   (`clipboard.copy-html`) adds HTML Format in the theme's colours; "Paste as one line"
   (`clipboard.paste-one-line`) and "Save output…" (`output.save`) have no default key and are
   in the pane menu (the Menu key, `Ctrl+right click`).
7. **The boundary is checked.** The Host and the CLI never call `subak_term::search`,
   `subak_term::links` or the window's modules (a source test, `tests/search_boundary.rs`);
   `deps-check` keeps GUI crates out of `subak` and `subak-term`; the release `subak.exe` does
   not import `ShellExecuteW` or `GetSaveFileNameW` and carries none of the regex engine (the
   linker drops what the Host never calls).

## Evidence (this machine: Windows 10 Pro 22H2 19045)

- Debug window, off-screen, a real Host: 15 000 rows printed by cmd.exe in 2.0 s; typing a
  9-character query (9 incremental searches) 207 ms; the last search read 15 006 lines in
  19.7 ms; a regex with 1 500 matches 36 ms; a dump round trip afterwards 0.6 ms
  (`gui_window.rs::a_large_history_stays_responsive_while_it_is_searched`).
- Headless, the largest history a pane keeps (4 MiB, 37 108 rows of coloured output): a full
  literal search **3.9 ms** and a regex with 8 571 matches **9.6 ms** in the release build (87 ms
  and 132 ms in the debug build). Patterns that hang backtracking engines (`(a*)*b`,
  `(a|aa)+$`) over 40 000 `a`s: 0.07–0.2 ms (release).
- Size: `subak-gui.exe` 1.75 → 2.97 MB (release), mostly the regex engine and its Unicode tables;
  `subak.exe` unchanged (2 100 224 bytes) and without `ShellExecuteW`, `GetSaveFileNameW`,
  `RegisterClipboardFormatW` or the regex engine's strings.
- The paste, link, export, selection and search flows, with exact bytes received by a stand-in
  program, in `gui_window.rs` (Step 19 tests) and `e2e/tests/step19_search_clipboard.rs`.

## Consequences

- A window can search only what its view holds: the Host's retention. Output that was evicted,
  cleared or never kept (the alternate screen's scrolled-off rows) cannot be found anywhere.
- The incremental refresh re-searches the screen on every batch of output while the bar is
  open; the time budget bounds it.
- The Host's model gained only read accessors (`history_text`, `history_wrapped`); nothing in
  the Host runs a search.
- Copy-on-select, right/middle click choices, configurable word separators, a setting to turn
  the paste question off, file-path links and dropped files are not part of this decision
  (settings are Step 20's).

## Not tested (and why)

The visible window, the real clipboard (other programs pasting the HTML Format), a real
browser opening a link and the real save dialog need a desktop session and the maintainer's
input; agents on this machine never show windows, use the real clipboard or open a browser.
The manual script is SUBAK_GUI §12.9.

## Alternatives considered

- A search in the Host (`pane.search`): rejected, see 1.
- Opening links with `cmd /c start` or `explorer.exe <url>`: rejected; a shell parses the
  string. `ShellExecuteW` with `open` hands the validated URL to its registered handler.
- Opening every link on a plain click (some terminals): rejected (`SUBAK-SEC-013`).
- A paste question for every multi-line paste even under bracketed paste: rejected as noise;
  bracketed paste is exactly the program saying it will not run lines as they arrive. Hidden
  control characters always ask, bracketed or not.
- Writing exports to a fixed folder without a dialog: rejected; the user chooses where.

## Requirements

`SUBAK-FR-047`, `SUBAK-SEC-013`, `SUBAK-SEC-014`, `SUBAK-FR-034`, `SUBAK-FR-036`,
`SUBAK-NFR-001`, `SUBAK-NFR-002`, `KIM-SEC-013`, `KIM-FR-012`.
