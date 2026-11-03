//! Where a binding is active (docs/HOTKEY_CONTRACT.md §4.1).

use std::fmt;

#[derive(Clone, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub enum Scope {
    /// Anywhere on the desktop while the tool runs.
    Global,
    /// One of the tool's windows has focus.
    App,
    /// A named context of the tool is active.
    Context(String),
}

impl Scope {
    /// `global`, `app` or `context:<id>` (the CLI's `scope_string`).
    pub fn parse(s: &str) -> Option<Scope> {
        match s {
            "global" => Some(Scope::Global),
            "app" => Some(Scope::App),
            _ => s
                .strip_prefix("context:")
                .filter(|c| crate::keys::is_name_id(c))
                .map(|c| Scope::Context(c.to_string())),
        }
    }

    /// Overlapping scopes within one tool (§9.3): `global` overlaps every scope, `app` overlaps
    /// `app`, a context overlaps itself.
    pub fn overlaps(&self, o: &Scope) -> bool {
        match (self, o) {
            (Scope::Global, _) | (_, Scope::Global) | (Scope::App, Scope::App) => true,
            (Scope::Context(a), Scope::Context(b)) => a == b,
            _ => false,
        }
    }

    /// The family used by `disabled_scopes`: `global`, `app` or `context`.
    pub fn family(&self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::App => "app",
            Scope::Context(_) => "context",
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scope::Global => f.write_str("global"),
            Scope::App => f.write_str("app"),
            Scope::Context(c) => write!(f, "context:{c}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_display_and_overlap() {
        for s in ["global", "app", "context:copy-mode"] {
            assert_eq!(Scope::parse(s).unwrap().to_string(), s);
        }
        assert!(Scope::parse("context:").is_none());
        assert!(Scope::parse("window").is_none());
        let ctx = |c: &str| Scope::Context(c.into());
        assert!(Scope::Global.overlaps(&ctx("a")));
        assert!(Scope::App.overlaps(&Scope::App));
        assert!(!Scope::App.overlaps(&ctx("a")));
        assert!(ctx("a").overlaps(&ctx("a")) && !ctx("a").overlaps(&ctx("b")));
    }
}
