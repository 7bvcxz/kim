# ADR 0034 — Ddalgak typing: worker runs, hotstrings from untouched key translation, a clipboard always given back

Status: **Accepted** · Date: 2026-11-04 · Step 69 · Refines [ADR 0022](0022-ddalgak-rule-model-and-safety.md)
(no character translation in the hook; hotstrings as their own trigger kind) and
[ADR 0031](0031-ddalgak-remap-execution-and-mouse-hook.md) §10 (`send_keys` arrives with typing) ·
Changes [DDALGAK_RULE_MODEL](../DDALGAK_RULE_MODEL.md) §3.4 (new), §5.3, §5.4, §8, §12 · Builds on
[ADR 0024](0024-ddalgak-windows-input-apis-and-permissions.md), [ADR 0028](0028-ddalgak-rule-store-validation-and-reload.md)

## Context

Step 69 makes `type_text` and `send_keys` run and adds hotstrings (typed abbreviations as
triggers). Typing is the most dangerous thing Ddalgak does: it puts characters into whatever
window has focus, possibly for many seconds, possibly through the user's clipboard. It must
never type into the wrong window, a password field or an elevated window, never leave a modifier
or the clipboard changed, never keep or show what the user typed, and stay cancellable by the
emergency chord, a pause and a focus change. Hotstrings need to know which *characters* the user
types, which ADR 0022 avoided ("translating keys to characters inside the hook changes dead-key
state in the focused application").

## Decision

1. **Typing runs are plans the worker carries out between its messages.** A fired `type_text` or
   `send_keys` rule (or a completed hotstring) becomes a small list of pieces: release the held
   modifiers, one `SendInput` call (32 characters, or one character with a key delay), a wait,
   copy or restore the clipboard, press the modifiers again. The worker does one piece, then
   handles its other messages (remap instructions, a pause, the emergency chord, a reload) before
   the next. Consequences: remaps keep working while text is typed; cancellation lands at the
   next piece (`cancelled`); a run has a timer only while it waits, so an idle engine still has
   none; the worker stays the single owner of the injector ledger (ADR 0031 §1). **One run at a
   time**: a trigger while a run is in progress is dropped with `busy` (stricter than "one run per
   rule": two snippets never interleave). A run longer than 60 s ends with `timeout`; the
   semantic check `run_too_long` also counts key and step delays as written (the xtask reference
   and the port change together).
2. **The target is checked before every call.** A read-only probe reports the foreground window
   and focused control, the session state, a password edit (`ES_PASSWORD`), the integrity level,
   an IME composition and whether a control can take text (focus exists, enabled, not
   `ES_READONLY`). Before the first piece a refusal skips the run (`skipped_secure_desktop`,
   `skipped_password_field`, `skipped_elevated_target`, `skipped_ime_composition`, new
   `skipped_disabled_field`); later, a different window or control ends it with the new
   `focus_changed` and **nothing more is typed** into the new window. A partial `SendInput` ends
   it with `injection_failed` and any key left down by the partial call is released at once.
3. **Text is Unicode key events, exactly as written.** One `KEYEVENTF_UNICODE` down and up per
   UTF-16 unit (emoji as two units, high surrogate first), no normalization (composed and
   decomposed Hangul arrive as written), `\n` and `\t` as the Enter and Tab keys. Characters that
   are syntax elsewhere (`{`, `^`, `+`) are plain text. `send_keys` taps each chord with its
   modifiers in one call.
4. **Modifiers.** The modifiers the application sees held are released logically before typing,
   with an unassigned masking key (`0xE8`) before an Alt release so no menu bar opens; Win is
   never released (a lone Win release opens Start; rules never fire with Win held). Afterwards
   Ctrl and Shift are pressed again only if still physically held (or held by a remap), re-checked
   once against a release that raced; Alt is never pressed again (a lone release would open the
   menu bar). The core consumes the physical release of every modifier a run released, so
   applications never see an unmatched up. The core shares this state, and a count of physical
   key presses, through atomics (numbers only).
5. **Paste gives the clipboard back on every path.** `method: paste` opens the clipboard with the
   app's hidden window as owner (its thread pumps messages, so other programs never wait on
   Ddalgak), copies every format (memory formats byte for byte, enhanced metafiles by their bits;
   formats Windows synthesizes are left to Windows), **refuses with `clipboard_not_restorable`
   and changes nothing** when a format cannot be copied faithfully or the content exceeds 32 MiB,
   puts the text on with the documented formats that keep it out of clipboard history and the
   cloud clipboard, presses Ctrl+V, waits 500 ms, and puts the copy back unless another program
   changed the clipboard meanwhile (its newer content wins). A busy clipboard is retried for
   200 ms (`clipboard_busy`, nothing pasted) and 500 ms when restoring
   (`clipboard_restore_failed`). Cancel, emergency, focus change and failures restore first.
6. **Hotstrings are a new trigger kind** (`trigger.hotstring` with `abbreviation`, `case`,
   `inside_words`, `immediate`, `end_chars`, `omit_end`, `erase`), additive to rules v1; an older
   reader keeps such a rule inactive (it lacks `keys`). Characters come from `ToUnicodeEx` for the
   foreground thread's layout **with flag 4, "keyboard state is not changed"** (Windows 10 1607
   and later, *documented*): the dead-key concern of ADR 0022 does not arise, so the hook may
   translate — only while a hotstring rule is active and only keys that reach the application
   unchanged. A dead key clears the buffer. The core keeps a **bounded buffer** of the last
   characters (the longest abbreviation plus one, at most 41), cleared on every reset: focus
   change, password field, IME composition, secure desktop, elevated window, pause, other
   programs' input, navigation and other non-text keys, Ctrl/Alt/Win shortcuts, IME keys, a rule
   taking the key, a match, a key-state reset and 10 s without typing. It is never stored, logged
   or shown. The key that completes an abbreviation is consumed (so Enter does not submit before
   the replacement); the run erases the abbreviation with Backspace, types the replacement (case
   conformed: `BTW` → upper, `Btw` → first letter), types the end key again with its modifiers
   unless `omit_end`, and gives the key back unchanged when nothing is typed (busy, a skip, a
   reload). If the user typed another key before the first Backspace, the run is `cancelled`
   without erasing. The IME's native mode (Hangul) is read from the focused window's IME window
   at expansion time and skips the hotstring (`skipped_ime_composition`). In Step 69 a hotstring
   runs exactly one `type_text` or `send_keys` step; other steps after a hotstring wait for
   ordered runs (Step 78, `step_unsupported`).
7. **Variables are a fixed whitelist**, only with `variables: true`: date and time
   (`{date}`, `{time}`, `{datetime}`, `{year}`…`{second}`, local time when the rule runs, fixed
   width) and one `{cursor}` insertion point (Left presses after the text, one per character
   after it); `{{` and `}}` type braces. No environment, clipboard or user variables. The schema
   enforces the grammar with a pattern the renderer is tested against.
8. **Snippet text never leaves the rule.** Validation messages describe what is wrong (length,
   position of a control character, position of a template error) without quoting the text;
   `rule show` replaces text with `{redacted, preview}` unless `--show-text`; default titles carry
   counts; activity, status and logs carry rule IDs and outcome codes only. The rules file and its
   history snapshots keep the text: that is the user's configuration (ADR 0028), not a transcript.
9. **The debug test host never types or touches the clipboard**: its worker sends nothing and its
   runs see a fixed target and a clipboard that is always busy.

## Consequences

- Between the hook consuming a hotstring's end key and the run's first Backspace there are a few
  milliseconds (the worker's wake-up); a key typed in that window cancels the expansion and its end
  key is not given back (giving it back would reorder the user's text). Human typing is far slower.
- Mouse clicks are not seen without a mouse hook, so a click inside the same field does not reset
  the buffer (AutoHotkey resets on clicks). A focus change does.
- Password fields drawn by browsers or custom controls are not recognized (best effort,
  DDALGAK_SAFETY §8); IMEs whose mode cannot be read count as not native.
- Typing into real applications, the Korean IME, the real clipboard, password fields, elevated
  windows and focus switches are **not tested** by the automated tests on the shared machine; the
  manual script is DDALGAK_ENGINE §12.

## Alternatives considered

- **Match on virtual keys without translation.** Layout-independent only for letters; punctuation
  abbreviations (`;sig`) and end characters would follow the US layout. Rejected for translation
  with the no-state-change flag.
- **A separate runner thread with its own injector.** Two ledgers, and remap outputs could
  interleave with a chunk. Rejected for pieces on the worker.
- **Let the end key through and erase it.** Enter would submit a chat message before the
  replacement arrives. Rejected: the completing key is consumed and given back when needed.
- **Queue triggers while a run is busy.** Text would appear seconds later, possibly in another
  window. Rejected for `busy`.
- **Paste without refusing unrestorable content.** Would silently lose a picture or an
  application's private format. Rejected for `clipboard_not_restorable`.
- **Environment or clipboard variables.** Would let a snippet pull other data into what it types;
  not in v1.

## Requirements

`DDALGAK-FR-011`, `DDALGAK-FR-012`, `DDALGAK-FR-070`, `DDALGAK-NFR-001`, `DDALGAK-SEC-001`,
`DDALGAK-SEC-002`, `DDALGAK-SEC-004`, `DDALGAK-SEC-005`, `DDALGAK-SEC-007`, `DDALGAK-SEC-011`.
