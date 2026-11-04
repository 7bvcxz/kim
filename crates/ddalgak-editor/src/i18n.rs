//! Korean and English labels (docs/DDALGAK_GUI_JOURNEYS.md §5, `DDALGAK-FR-053`).
//!
//! Every label of the editor's chrome exists in both languages; the table is checked by a test
//! (unique keys, both texts present, the journeys' glossary words used). Sentences built by the
//! engine's data models (the rule summaries of Steps 68–72) are English for now; the Korean UI
//! shows them unchanged (DDALGAK_GUI_JOURNEYS §7).

/// The UI language.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Lang {
    #[default]
    En,
    Ko,
}

impl Lang {
    /// `en` or `ko` (`--lang`); anything else is `None`.
    pub fn parse(s: &str) -> Option<Lang> {
        match s.to_ascii_lowercase().as_str() {
            "en" | "english" => Some(Lang::En),
            "ko" | "kr" | "korean" | "한국어" => Some(Lang::Ko),
            _ => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ko => "ko",
        }
    }

    /// The other language (the header's language switch).
    pub fn other(self) -> Lang {
        match self {
            Lang::En => Lang::Ko,
            Lang::Ko => Lang::En,
        }
    }

    /// The label for `key`; the key itself when it is missing (a test keeps that from happening).
    pub fn t(self, key: &str) -> &'static str {
        match LABELS.iter().find(|(k, _, _)| *k == key) {
            Some((_, en, ko)) => match self {
                Lang::En => en,
                Lang::Ko => ko,
            },
            None => "?",
        }
    }
}

/// `(key, English, 한국어)`.
pub const LABELS: &[(&str, &str, &str)] = &[
    ("app.title", "Ddalgak rules", "딸깍 규칙"),
    ("lang.switch", "한국어", "English"),
    ("nav.rules", "Rules", "규칙"),
    ("nav.modes", "Modes", "모드"),
    ("engine.running", "● Running", "● 실행 중"),
    ("engine.paused", "◐ Paused", "◐ 일시 정지됨"),
    ("engine.off", "○ Off", "○ 꺼짐"),
    (
        "engine.not_running",
        "○ Ddalgak is not running",
        "○ 딸깍이 실행 중이 아님",
    ),
    ("engine.unknown", "… Asking Ddalgak", "… 딸깍에 묻는 중"),
    ("engine.turn_on", "Turn on", "켜기"),
    ("engine.turn_off", "Turn off", "끄기"),
    ("engine.pause", "Pause", "일시 정지"),
    ("engine.resume", "Resume", "다시 시작"),
    ("header.profile", "Profile", "프로필"),
    ("header.mode", "Mode", "모드"),
    (
        "header.emergency",
        "Emergency off: Ctrl+Alt+Shift+Esc (always works)",
        "긴급 끄기: Ctrl+Alt+Shift+Esc (항상 작동)",
    ),
    ("list.search", "Search rules…", "규칙 검색…"),
    ("list.search_label", "Search rules", "규칙 검색"),
    (
        "list.snippet_search",
        "Also search snippet text",
        "글자 내용도 검색",
    ),
    ("list.new", "+ New rule", "+ 새 규칙"),
    (
        "list.empty",
        "No rules yet. Ddalgak does nothing until you make one.",
        "아직 규칙이 없습니다. 규칙을 만들기 전에는 딸깍은 아무것도 하지 않습니다.",
    ),
    (
        "list.no_match",
        "No rule matches the search and filters.",
        "검색과 필터에 맞는 규칙이 없습니다.",
    ),
    (
        "list.first_rule",
        "Make your first rule:",
        "첫 규칙 만들기:",
    ),
    (
        "list.starter_remap",
        "CapsLock acts as Ctrl",
        "CapsLock을 Ctrl로",
    ),
    ("list.starter_text", "Type a phrase", "문구 입력"),
    ("list.starter_program", "Open a program", "프로그램 열기"),
    (
        "list.drag",
        "Drag to reorder (or Alt+Up / Alt+Down)",
        "끌어서 순서 바꾸기 (또는 Alt+Up / Alt+Down)",
    ),
    ("filter.state", "Show", "표시"),
    ("filter.all", "All rules", "모든 규칙"),
    ("filter.enabled", "Enabled", "켜짐"),
    ("filter.disabled", "Disabled", "꺼짐"),
    ("filter.review", "Needs review", "검토 필요"),
    ("filter.problems", "Has problems", "문제 있음"),
    ("filter.profile", "Profile", "프로필"),
    ("filter.mode", "Mode", "모드"),
    ("filter.app", "App", "앱"),
    ("filter.any", "Any", "전체"),
    ("group.label", "Group", "묶기"),
    ("group.none", "No grouping", "묶지 않음"),
    ("group.profile", "By profile", "프로필별"),
    ("group.mode", "By mode", "모드별"),
    ("group.app", "By app", "앱별"),
    ("group.every_mode", "Every mode", "모든 모드"),
    ("group.every_app", "Every app", "모든 앱"),
    ("bulk.selected", "selected", "개 선택됨"),
    ("bulk.select_all", "Select all", "모두 선택"),
    ("bulk.clear", "Clear selection", "선택 해제"),
    (
        "bulk.hint",
        "Several rules are selected: the actions for all of them are above the list.",
        "여러 규칙이 선택됨: 모두에 대한 동작은 목록 위에 있습니다.",
    ),
    ("badge.conflict", "conflict", "충돌"),
    ("badge.problem", "problem", "문제"),
    ("badge.warning", "warning", "경고"),
    ("badge.review", "needs review", "검토 필요"),
    ("badge.inactive", "inactive", "작동 안 함"),
    ("state.active", "active", "작동 중"),
    (
        "state.ready",
        "saved, not active yet",
        "저장됨, 아직 작동 안 함",
    ),
    ("state.disabled", "off", "꺼짐"),
    ("state.other_mode", "in another mode", "다른 모드에서"),
    ("state.other_profile", "other profile", "다른 프로필"),
    ("btn.edit", "Edit", "편집"),
    ("btn.test", "Test", "시험"),
    ("btn.duplicate", "Duplicate", "복제"),
    ("btn.delete", "Delete", "삭제"),
    ("btn.enable", "Enable", "켜기"),
    ("btn.disable", "Disable", "끄기"),
    ("btn.undo", "Undo", "실행 취소"),
    ("btn.dismiss", "Dismiss", "닫기"),
    ("btn.export", "Export…", "내보내기…"),
    ("btn.import", "Import…", "가져오기…"),
    ("btn.cancel", "Cancel", "취소"),
    ("btn.save", "Save", "저장"),
    ("btn.save_enable", "Save and enable", "저장하고 켜기"),
    ("btn.save_off", "Save turned off", "꺼진 채로 저장"),
    ("btn.back", "◂ Back", "◂ 이전"),
    ("btn.next", "Next ▸", "다음 ▸"),
    ("btn.allow", "Allow", "허용"),
    ("btn.keep_editing", "Keep editing", "계속 편집"),
    ("btn.discard", "Discard changes", "변경 내용 버리기"),
    ("btn.close", "Close", "닫기"),
    ("btn.revert", "Revert", "되돌리기"),
    ("btn.add_disabled", "Add them turned off", "꺼진 채로 추가"),
    ("btn.reload", "Load the newer rules", "새 규칙 불러오기"),
    ("form.new", "New rule", "새 규칙"),
    ("form.edit", "Edit rule", "규칙 편집"),
    ("form.step.keys", "1 Keys", "1 키"),
    ("form.step.effect", "2 Effect", "2 동작"),
    ("form.step.details", "3 Details", "3 세부 설정"),
    ("form.step.try", "4 Try", "4 시험"),
    ("form.when_press", "When I press", "이 키를 누르면"),
    ("form.when_type", "When I type", "이렇게 입력하면"),
    ("form.capture", "Press the keys…", "키를 누르세요…"),
    (
        "form.capture_label",
        "Key capture: press the keys for this rule",
        "키 입력: 이 규칙의 키를 누르세요",
    ),
    (
        "form.capture_hint",
        "Esc cancels, Tab leaves. Keys Windows takes first: use Type instead.",
        "Esc는 취소, Tab은 나가기. Windows가 먼저 가져가는 키는 직접 입력하세요.",
    ),
    ("form.type_instead", "Type instead", "직접 입력"),
    (
        "form.special_keys",
        "Keys that cannot be pressed here:",
        "여기서 누를 수 없는 키:",
    ),
    ("form.on_press", "When pressed", "누를 때"),
    ("form.on_release", "When tapped alone", "혼자 눌렀다 뗄 때"),
    ("form.on_hold", "When held", "누르고 있을 때"),
    ("form.hold_ms", "Held for (ms)", "누르는 시간 (ms)"),
    (
        "form.pass_through",
        "Also let the key through to the app",
        "앱에도 키 전달",
    ),
    ("form.abbreviation", "Abbreviation", "줄임말"),
    ("form.title", "Name", "이름"),
    ("form.description", "Description (optional)", "설명 (선택)"),
    ("form.do", "…do this", "…이렇게 하기"),
    ("form.more", "More:", "더 보기:"),
    (
        "form.advanced",
        "Advanced: modes, apps, priority",
        "고급: 모드, 앱, 우선순위",
    ),
    (
        "form.only_modes",
        "Only in modes (none ticked: every mode)",
        "이 모드에서만 (선택 없음: 모든 모드)",
    ),
    (
        "form.only_apps",
        "Only in apps (file names, one per line)",
        "이 앱에서만 (파일 이름, 한 줄에 하나)",
    ),
    (
        "form.apps_note",
        "App conditions are evaluated from Step 77; until then a rule with one stays inactive (condition_unsupported).",
        "앱 조건은 Step 77부터 적용됩니다. 그때까지 앱 조건이 있는 규칙은 작동하지 않습니다 (condition_unsupported).",
    ),
    ("form.priority", "Priority", "우선순위"),
    ("form.summary", "Summary", "요약"),
    ("form.preview", "Preview", "미리보기"),
    ("form.permission", "Permission", "권한"),
    (
        "form.not_editable",
        "This rule cannot be edited in the form",
        "이 규칙은 양식에서 편집할 수 없습니다",
    ),
    (
        "form.try_intro",
        "Try it here: simulation only, nothing leaves this window.",
        "여기서 시험: 시뮬레이션만, 이 창 밖으로 아무것도 나가지 않습니다.",
    ),
    ("form.simulate", "Simulate", "시뮬레이션"),
    (
        "form.changed_elsewhere",
        "The rules changed outside the editor.",
        "규칙이 편집기 밖에서 바뀌었습니다.",
    ),
    ("tile.remap", "Act as another key", "다른 키로 바꾸기"),
    ("tile.remap.example", "CapsLock → Ctrl", "CapsLock → Ctrl"),
    ("tile.send_keys", "Press a shortcut", "단축키 누르기"),
    ("tile.send_keys.example", "F13 → Ctrl+C", "F13 → Ctrl+C"),
    ("tile.type_text", "Type text", "글자 입력"),
    (
        "tile.type_text.example",
        "\"감사합니다.\"",
        "\"감사합니다.\"",
    ),
    ("tile.open_program", "Open a program", "프로그램 열기"),
    ("tile.open_program.example", "Notepad", "메모장"),
    ("tile.open_url", "Open a website", "웹사이트 열기"),
    ("tile.open_url.example", "github.com", "github.com"),
    ("tile.mouse", "Mouse", "마우스"),
    (
        "tile.mouse.example",
        "move · scroll · click",
        "이동 · 스크롤 · 클릭",
    ),
    ("tile.mode", "Switch mode", "모드 전환"),
    ("tile.mode.example", "Navigation", "탐색 모드"),
    ("tile.engine", "Pause Ddalgak", "딸깍 일시 정지"),
    (
        "tile.engine.example",
        "pause · resume · off",
        "일시 정지 · 다시 시작 · 끄기",
    ),
    ("tile.block", "Block this key", "이 키 막기"),
    (
        "tile.block.example",
        "Insert does nothing",
        "Insert가 아무것도 안 함",
    ),
    ("field.acts_as", "Acts as", "바뀔 키"),
    (
        "field.tap",
        "A quick tap alone sends",
        "짧게 혼자 누르면 보낼 키",
    ),
    ("field.tap_ms", "Tap time (ms)", "짧게 누름 시간 (ms)"),
    (
        "field.shortcut",
        "Shortcut (up to eight chords)",
        "단축키 (최대 여덟 개)",
    ),
    (
        "field.step_delay",
        "Delay between chords (ms)",
        "단축키 사이 지연 (ms)",
    ),
    ("field.text", "Text", "글자"),
    (
        "field.variables",
        "Fill in variables ({date}, {time}, {cursor}…)",
        "변수 채우기 ({date}, {time}, {cursor}…)",
    ),
    (
        "field.paste",
        "Paste instead of typing (needs the clipboard permission)",
        "입력 대신 붙여넣기 (클립보드 권한 필요)",
    ),
    (
        "field.key_delay",
        "Delay between characters (ms)",
        "글자 사이 지연 (ms)",
    ),
    ("field.program", "Program (.exe)", "프로그램 (.exe)"),
    ("field.app_id", "Or an app ID", "또는 앱 ID"),
    (
        "field.args",
        "Arguments (one per line)",
        "인수 (한 줄에 하나)",
    ),
    (
        "field.start_in",
        "Start in (empty: the program's folder)",
        "시작 폴더 (비우면 프로그램 폴더)",
    ),
    ("field.window", "Window", "창"),
    ("field.window.normal", "Normal", "보통"),
    ("field.window.maximized", "Maximized", "최대화"),
    ("field.window.minimized", "Minimized", "최소화"),
    (
        "field.activate",
        "Bring it to the front if it is already running",
        "이미 실행 중이면 앞으로 가져오기",
    ),
    ("field.url", "Website", "웹사이트"),
    ("field.mouse_kind", "Mouse", "마우스"),
    ("field.mouse.move_by", "Move by", "만큼 이동"),
    ("field.mouse.move_to", "Move to", "위치로 이동"),
    ("field.mouse.scroll", "Scroll", "스크롤"),
    ("field.mouse.click", "Click", "클릭"),
    (
        "field.right",
        "Right (negative: left)",
        "오른쪽 (음수: 왼쪽)",
    ),
    ("field.down", "Down (negative: up)", "아래 (음수: 위)"),
    (
        "field.while_held",
        "While the keys are held",
        "키를 누르고 있는 동안",
    ),
    ("field.hold.once", "Once per press", "누를 때마다 한 번"),
    (
        "field.hold.glide",
        "Keep moving smoothly",
        "부드럽게 계속 이동",
    ),
    ("field.hold.repeat", "Repeat every", "반복 간격"),
    ("field.speed", "Speed (px/s)", "속도 (px/s)"),
    ("field.repeat_ms", "Repeat every (ms)", "반복 간격 (ms)"),
    ("field.x", "x", "x"),
    ("field.y", "y", "y"),
    ("field.relative_to", "Relative to", "기준"),
    ("field.direction", "Direction", "방향"),
    ("field.notches", "Notches", "칸 수"),
    ("field.button", "Button", "버튼"),
    ("field.click_op", "Click", "클릭 방식"),
    ("field.switch", "Switch", "전환"),
    ("field.mode", "Mode", "모드"),
    ("field.cycle_modes", "Modes, in order", "모드 순서"),
    ("field.engine_op", "What happens", "동작"),
    (
        "field.engine_note",
        "No rule can turn Ddalgak on; only you can.",
        "어떤 규칙도 딸깍을 켤 수 없습니다. 켜는 것은 사용자만 할 수 있습니다.",
    ),
    ("dialog.delete", "Delete rules", "규칙 삭제"),
    (
        "dialog.discard",
        "Discard your changes?",
        "변경 내용을 버릴까요?",
    ),
    (
        "dialog.discard_text",
        "The rule you are editing has changes that are not saved.",
        "편집 중인 규칙에 저장하지 않은 변경 내용이 있습니다.",
    ),
    (
        "dialog.consent",
        "Allow this rule?",
        "이 규칙을 허용할까요?",
    ),
    (
        "dialog.consent_text",
        "Ddalgak asks once per rule. You can withdraw it later.",
        "딸깍은 규칙마다 한 번 묻습니다. 나중에 거둘 수 있습니다.",
    ),
    ("dialog.export", "Export rules", "규칙 내보내기"),
    (
        "dialog.export_text",
        "Writes a portable rules file without permissions or settings.",
        "권한이나 설정 없이 옮길 수 있는 규칙 파일을 씁니다.",
    ),
    ("dialog.import", "Import rules", "규칙 가져오기"),
    (
        "dialog.import_text",
        "Imported rules are added turned off and need your review. Nothing is opened or run.",
        "가져온 규칙은 꺼진 채로 추가되며 검토가 필요합니다. 아무것도 열거나 실행하지 않습니다.",
    ),
    ("dialog.file", "File", "파일"),
    ("dialog.check", "Check the file", "파일 확인"),
    ("dialog.help", "Keyboard shortcuts", "키보드 단축키"),
    (
        "perm.input-injection",
        "Let this rule press keys, type or move the mouse in other programs?",
        "이 규칙이 다른 프로그램에서 키를 누르거나 글자를 입력하거나 마우스를 움직이도록 허용할까요?",
    ),
    (
        "perm.launch-programs",
        "Let this rule open this program or website?",
        "이 규칙이 이 프로그램이나 웹사이트를 열도록 허용할까요?",
    ),
    (
        "perm.clipboard",
        "Let this rule use the clipboard (it is restored afterwards)?",
        "이 규칙이 클립보드를 쓰도록 허용할까요? (나중에 되돌립니다)",
    ),
    ("perm.short.input-injection", "types for you", "대신 입력"),
    (
        "perm.short.launch-programs",
        "opens programs",
        "프로그램 열기",
    ),
    ("perm.short.clipboard", "clipboard", "클립보드"),
    ("perm.missing", "needs your approval", "승인 필요"),
    ("status.rules", "rules", "개 규칙"),
    ("status.problems", "with problems", "개 문제"),
    ("status.review", "need review", "개 검토 필요"),
    ("status.draft", "draft: not saved", "초안: 저장 안 됨"),
    ("status.saving", "saving…", "저장 중…"),
    (
        "status.waiting",
        "saved; waiting for Ddalgak to load it",
        "저장됨; 딸깍이 불러오기를 기다리는 중",
    ),
    ("status.active", "saved and active", "저장되고 작동 중"),
    (
        "status.loaded_off",
        "saved and loaded; the engine is off",
        "저장되고 불러옴; 엔진이 꺼져 있음",
    ),
    (
        "status.loaded_paused",
        "saved and loaded; the engine is paused",
        "저장되고 불러옴; 엔진이 일시 정지됨",
    ),
    (
        "status.not_running",
        "saved; Ddalgak is not running, so nothing is active",
        "저장됨; 딸깍이 실행 중이 아니라 작동하는 것이 없음",
    ),
    (
        "status.kept_previous",
        "saved; Ddalgak kept its previous rules",
        "저장됨; 딸깍이 이전 규칙을 유지함",
    ),
    (
        "status.unconfirmed",
        "saved; Ddalgak did not confirm the reload",
        "저장됨; 딸깍이 다시 불러오기를 확인하지 않음",
    ),
    ("status.revision", "revision", "리비전"),
    ("toast.deleted", "Deleted", "삭제됨"),
    ("toast.undone", "Undone", "실행 취소됨"),
    ("help.f1", "F1: shortcuts and help", "F1: 단축키와 도움말"),
    (
        "help.off",
        "The rule list's shortcuts are off (--no-hotkeys or KIM_NO_HOTKEYS).",
        "규칙 목록 단축키가 꺼져 있습니다 (--no-hotkeys 또는 KIM_NO_HOTKEYS).",
    ),
    (
        "help.rebind",
        "Change the rule list's shortcuts in hotkeys.json (HOTKEY_CONTRACT); the navigation keys are fixed.",
        "규칙 목록 단축키는 hotkeys.json에서 바꿉니다 (HOTKEY_CONTRACT). 이동 키는 고정입니다.",
    ),
    (
        "engine.start_hint",
        "Start Ddalgak from the Start menu; the editor never starts it.",
        "시작 메뉴에서 딸깍을 실행하세요. 편집기는 딸깍을 실행하지 않습니다.",
    ),
    (
        "detail.select_hint",
        "Select a rule, or press Ctrl+N for a new one.",
        "규칙을 고르거나 Ctrl+N으로 새 규칙을 만드세요.",
    ),
    ("detail.state", "State", "상태"),
    ("dialog.export_done", "Exported to", "내보낸 곳:"),
    (
        "dialog.import_adds",
        "rules would be added, turned off, to",
        "개 규칙이 꺼진 채로 추가될 프로필:",
    ),
    (
        "dialog.import_opens",
        "opens once you approve and enable it",
        "승인하고 켜면 여는 대상",
    ),
    (
        "dialog.import_injects",
        "presses keys or types in other programs once you approve and enable it",
        "승인하고 켜면 다른 프로그램에서 키를 누르거나 입력함",
    ),
    (
        "dialog.import_renamed",
        "ID taken, renamed:",
        "ID가 이미 있어 이름 변경:",
    ),
    ("dialog.import_modes", "New modes", "새 모드"),
    (
        "field.at_point",
        "Move to a point first",
        "먼저 위치로 이동",
    ),
    ("field.columns", "Columns", "열"),
    ("field.rows", "Rows", "행"),
    (
        "field.unit_grid",
        "Count in grid cells instead of pixels",
        "픽셀 대신 격자 칸으로 세기",
    ),
    ("field.new_mode", "New mode name", "새 모드 이름"),
    (
        "form.add_step",
        "Add step: the next keys continue a sequence (up to four)",
        "단계 추가: 다음 키가 순서를 이어감 (최대 네 개)",
    ),
    (
        "form.as_typed",
        "As it would be typed now (in this window only)",
        "지금 입력될 내용 (이 창 안에서만)",
    ),
    (
        "form.clamped",
        "The point is beside every monitor; the pointer goes to the nearest edge.",
        "위치가 모든 모니터 밖에 있어 포인터가 가장 가까운 가장자리로 갑니다.",
    ),
    ("form.command_line", "Command line", "명령줄"),
    ("form.enabled", "Enabled", "켜짐"),
    ("form.found", "found", "찾음"),
    (
        "form.not_found",
        "not found or not a program on this PC",
        "이 PC에 없거나 프로그램이 아님",
    ),
    (
        "form.held_modifiers",
        "Held modifiers combine:",
        "누른 보조 키와 함께:",
    ),
    (
        "form.ime",
        "An input method is composing: switch to English input, or type the keys instead.",
        "입력기가 조합 중입니다. 영문 입력으로 바꾸거나 키를 직접 입력하세요.",
    ),
    (
        "form.plain_note",
        "rules.json keeps the text in plain form: not for passwords.",
        "rules.json에는 글자가 그대로 저장됩니다. 비밀번호에는 쓰지 마세요.",
    ),
    (
        "form.program_note",
        "Nothing opens here. Ddalgak opens it as you, never as administrator, after you allow it.",
        "여기서는 아무것도 열리지 않습니다. 허용한 뒤에 관리자 권한 없이 사용자로 엽니다.",
    ),
    (
        "form.screen_map",
        "Screen map (simulated pointer; the real pointer never moves here)",
        "화면 지도 (시뮬레이션 포인터, 실제 포인터는 움직이지 않음)",
    ),
    ("form.try_keys", "Use", "사용:"),
    ("modes.add", "+ New mode", "+ 새 모드"),
    ("modes.default", "default", "기본"),
    ("modes.layer_over", "layer over", "위에 겹침:"),
    (
        "modes.note",
        "Rename, colors, layers and deleting modes: `ddalgak mode …` for now.",
        "모드 이름 변경, 색, 겹침, 삭제: 지금은 `ddalgak mode …`를 쓰세요.",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_label_has_both_languages_and_a_unique_key() {
        let mut seen = BTreeSet::new();
        for (k, en, ko) in LABELS {
            assert!(seen.insert(*k), "duplicate key {k}");
            assert!(!en.trim().is_empty() && !ko.trim().is_empty(), "{k}");
        }
        assert_eq!(Lang::Ko.t("nav.rules"), "규칙");
        assert_eq!(Lang::En.t("nav.rules"), "Rules");
        assert_eq!(Lang::En.t("no.such.key"), "?");
    }

    /// The glossary of DDALGAK_GUI_JOURNEYS §5 is used word for word.
    #[test]
    fn the_journeys_glossary_is_used() {
        for (key, en, ko) in [
            ("tile.remap", "Act as another key", "다른 키로 바꾸기"),
            ("tile.send_keys", "Press a shortcut", "단축키 누르기"),
            ("tile.type_text", "Type text", "글자 입력"),
            ("tile.open_program", "Open a program", "프로그램 열기"),
            ("tile.open_url", "Open a website", "웹사이트 열기"),
            ("tile.mouse", "Mouse", "마우스"),
            ("tile.mode", "Switch mode", "모드 전환"),
            ("tile.engine", "Pause Ddalgak", "딸깍 일시 정지"),
            ("form.when_press", "When I press", "이 키를 누르면"),
            ("form.simulate", "Simulate", "시뮬레이션"),
            ("header.profile", "Profile", "프로필"),
            ("nav.rules", "Rules", "규칙"),
        ] {
            assert_eq!(Lang::En.t(key), en);
            assert_eq!(Lang::Ko.t(key), ko);
        }
        assert!(Lang::Ko.t("header.emergency").contains("긴급 끄기"));
    }

    /// Every literal key the window asks for (`.t("…")`) is in the table, so no label shows "?".
    #[test]
    fn every_key_the_window_uses_exists() {
        let src = [include_str!("app.rs"), include_str!("app/view.rs")].concat();
        let keys: BTreeSet<&str> = LABELS.iter().map(|(k, _, _)| *k).collect();
        let mut missing = Vec::new();
        for part in src.split(".t(").skip(1) {
            let part = part.trim_start();
            let Some(rest) = part.strip_prefix('"') else {
                continue; // a computed key (`format!`), checked below
            };
            let key = &rest[..rest.find('"').unwrap_or(0)];
            if !keys.contains(key) {
                missing.push(key.to_string());
            }
        }
        assert!(missing.is_empty(), "missing labels: {missing:?}");
        // The computed keys: tiles, their examples, permissions.
        for e in crate::form::Effect::TILES
            .iter()
            .chain([crate::form::Effect::Block].iter())
        {
            assert!(keys.contains(format!("tile.{}", e.key()).as_str()));
            assert!(keys.contains(format!("tile.{}.example", e.key()).as_str()));
        }
        for p in ddalgak::model::Permission::ALL {
            assert!(keys.contains(format!("perm.{}", p.as_str()).as_str()));
            assert!(keys.contains(format!("perm.short.{}", p.as_str()).as_str()));
        }
        for w in ["normal", "maximized", "minimized"] {
            assert!(keys.contains(format!("field.window.{w}").as_str()));
        }
    }

    #[test]
    fn languages_parse_and_switch() {
        assert_eq!(Lang::parse("ko"), Some(Lang::Ko));
        assert_eq!(Lang::parse("EN"), Some(Lang::En));
        assert_eq!(Lang::parse("fr"), None);
        assert_eq!(Lang::Ko.other(), Lang::En);
    }
}
