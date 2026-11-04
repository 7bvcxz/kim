//! Annotation — **PLACEHOLDER (not implemented).**
//!
//! Annotation (shapes, text with IME, undo/redo, mosaic, blur, redaction) is milestone N3,
//! Steps 56, 57 and 59 (NOEUL-FR-080…084). This module only fixes where it will live and what it
//! will own, so the capture and pin modules can be written against it: annotations will be
//! editable objects attached to a capture session or a pin, flattened into the picture only on
//! copy, save or an explicit flatten (NOEUL-FR-081), and a solid redaction will leave nothing of
//! the original in exported pixels (NOEUL-FR-082).
//!
//! Nothing in this build draws, edits or registers an annotation action: the shipped hotkey
//! manifest contains no `annotate.*` action (NOEUL-FR-114), and no CLI command mentions it
//! except as planned.

/// Whether annotation exists in this build. Always `false` until Step 56.
pub const IMPLEMENTED: bool = false;

/// The Step that implements annotation, for `--help` and status output.
pub const PLANNED_STEP: u32 = 56;

#[cfg(test)]
mod tests {
    #[test]
    fn annotation_is_labeled_as_not_implemented() {
        const { assert!(!super::IMPLEMENTED) };
        let manifest: serde_json::Value =
            serde_json::from_str(crate::hotkeys::MANIFEST_JSON).unwrap();
        assert!(
            manifest["actions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|a| !a["id"].as_str().unwrap().starts_with("annotate.")),
            "no annotation action is shipped before it exists (NOEUL-FR-114)"
        );
    }
}
