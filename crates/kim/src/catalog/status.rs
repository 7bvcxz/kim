//! Per-tool status for the current machine (docs/CATALOG_FORMAT.md §6). Pure evaluation: it
//! reads install records but never changes anything.

use super::{Asset, Catalog, Channel, Release, Support, Tool};
use crate::paths::Paths;
use crate::platform::Platform;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolStatus {
    /// Installed and current for the selected channel.
    Installed,
    /// Installed; a newer compatible release exists. Shown only — never applied automatically.
    UpdateAvailable,
    /// Not installed; a compatible release exists and can be installed on explicit request.
    Available,
    /// Releases exist, but none is compatible with this machine or Kim version.
    Incompatible,
    /// No release on the selected channel yet.
    Unreleased,
    /// An install record exists but cannot be read.
    Damaged,
}

/// How far a tool's distribution has come, derived from the signed catalog's releases and never
/// declared by it (CATALOG_FORMAT §6, `KIM-FR-051`): a tool is only *available* once a signed,
/// hashed stable release exists. Independent of this machine (that is [`ToolStatus`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Availability {
    /// No release on any channel: listed so users know it exists, nothing to download.
    Planned,
    /// Only preview-channel releases exist.
    Preview,
    /// At least one stable release exists.
    Available,
}

impl Availability {
    pub fn of(tool: &Tool) -> Self {
        if tool.releases.iter().any(|r| r.channel == Channel::Stable) {
            Self::Available
        } else if tool.releases.is_empty() {
            Self::Planned
        } else {
            Self::Preview
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Preview => "preview",
            Self::Available => "available",
        }
    }
}

/// What is installed for a tool, as far as Kim can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    No,
    Version(semver::Version),
    Damaged(String),
}

/// Contents of `<programs>\<tool>\current.json` (written by the installer, Step 06).
#[derive(Debug, Deserialize)]
struct InstallRecord {
    version: String,
}

/// Reads the install record of `tool_id`. Kim itself is "installed" as the running build.
pub fn installed(paths: &Paths, tool_id: &str) -> Installed {
    if tool_id == "kim" {
        return semver::Version::parse(crate::KIM_VERSION)
            .map_or(Installed::No, Installed::Version);
    }
    let path = paths.install_record(tool_id);
    if !path.exists() {
        return Installed::No;
    }
    let parsed = std::fs::read(&path)
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice::<InstallRecord>(&b).map_err(|e| e.to_string()))
        .and_then(|r| semver::Version::parse(&r.version).map_err(|e| e.to_string()));
    match parsed {
        Ok(v) => Installed::Version(v),
        Err(e) => Installed::Damaged(format!("{}: {e}", path.display())),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolView {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub support: Support,
    pub availability: Availability,
    pub status: ToolStatus,
    pub installed_version: Option<String>,
    /// Newest release on the selected channel(s) that is compatible with this machine.
    pub latest_compatible: Option<String>,
    /// Newest release on the selected channel(s), compatible or not.
    pub latest: Option<String>,
    /// Why the newest release cannot be used here, if it cannot.
    pub incompatible_reason: Option<String>,
    /// Extra information, e.g. that only preview releases exist.
    pub note: Option<String>,
}

impl ToolView {
    /// The short word Kim and Kim Manager show for a tool that is not installed, so that the
    /// release stage is never hidden (Planned / Preview / Available, `KIM-FR-051`). `None` for
    /// installed and damaged tools, which have their own words.
    pub fn availability_label(&self) -> Option<&'static str> {
        match (self.status, self.availability) {
            (ToolStatus::Available, Availability::Preview) => Some("available (preview)"),
            (ToolStatus::Available, _) => Some("available"),
            (ToolStatus::Incompatible, _) => Some("incompatible"),
            (ToolStatus::Unreleased, Availability::Planned) => Some("planned"),
            (ToolStatus::Unreleased, Availability::Preview) => Some("preview only"),
            (ToolStatus::Unreleased, Availability::Available) => Some("not released"),
            (ToolStatus::Installed | ToolStatus::UpdateAvailable | ToolStatus::Damaged, _) => None,
        }
    }
}

/// Which channels to consider: `stable` only, or `preview` (stable + preview).
pub fn channel_allowed(selected: Channel, release: Channel) -> bool {
    release <= selected
}

/// Why `release` cannot be installed here, or `None` if it can.
pub fn incompatibility(
    release: &Release,
    platform: &Platform,
    kim: &semver::Version,
) -> Option<String> {
    if let Some(min) = &release.min_kim_version
        && let Ok(min) = semver::Version::parse(min)
        && *kim < min
    {
        return Some(format!("requires Kim {min} or newer (this is {kim})"));
    }
    if matching_asset(release, platform).is_some() {
        return None;
    }
    let same_target: Vec<&Asset> = release
        .assets
        .iter()
        .filter(|a| a.os == platform.os && a.arch == platform.arch)
        .collect();
    if let Some(a) = same_target.first()
        && let Some(min) = a.min_os_build
    {
        return Some(match platform.os_build {
            Some(b) => format!("requires {} build {min} or newer (this is {b})", a.os),
            None => format!("requires {} build {min} or newer (unknown here)", a.os),
        });
    }
    let targets: Vec<String> = release
        .assets
        .iter()
        .map(|a| format!("{}/{}", a.os, a.arch))
        .collect();
    Some(format!(
        "no package for {}/{} (available: {})",
        platform.os,
        platform.arch,
        targets.join(", ")
    ))
}

/// The asset of `release` that fits `platform`, if any.
pub fn matching_asset<'a>(release: &'a Release, platform: &Platform) -> Option<&'a Asset> {
    release.assets.iter().find(|a| {
        a.os == platform.os
            && a.arch == platform.arch
            && match (a.min_os_build, platform.os_build) {
                (None, _) => true,
                (Some(min), Some(b)) => b >= min,
                (Some(_), None) => false,
            }
    })
}

fn newest<'a>(
    releases: impl Iterator<Item = &'a Release>,
) -> Option<(&'a Release, semver::Version)> {
    releases
        .filter_map(|r| semver::Version::parse(&r.version).ok().map(|v| (r, v)))
        .max_by(|a, b| a.1.cmp(&b.1))
}

pub fn evaluate_tool(
    tool: &Tool,
    platform: &Platform,
    kim: &semver::Version,
    installed: &Installed,
    channel: Channel,
) -> ToolView {
    let on_channel = || {
        tool.releases
            .iter()
            .filter(|r| channel_allowed(channel, r.channel))
    };
    let latest = newest(on_channel());
    let latest_compatible =
        newest(on_channel().filter(|r| incompatibility(r, platform, kim).is_none()));
    let incompatible_reason = latest
        .as_ref()
        .and_then(|(r, _)| incompatibility(r, platform, kim));
    let note = (latest.is_none() && !tool.releases.is_empty() && channel == Channel::Stable)
        .then(|| "only preview releases exist; use --channel preview".to_string());

    let installed_version = match installed {
        Installed::Version(v) => Some(v.to_string()),
        _ => None,
    };
    let status = match installed {
        Installed::Damaged(_) => ToolStatus::Damaged,
        Installed::Version(v) => match &latest_compatible {
            Some((_, newest)) if newest > v => ToolStatus::UpdateAvailable,
            _ => ToolStatus::Installed,
        },
        Installed::No => match (&latest, &latest_compatible) {
            (_, Some(_)) => ToolStatus::Available,
            (Some(_), None) => ToolStatus::Incompatible,
            (None, None) => ToolStatus::Unreleased,
        },
    };
    ToolView {
        id: tool.id.clone(),
        name: tool.name.clone(),
        summary: tool.summary.clone(),
        support: tool.support,
        availability: Availability::of(tool),
        status,
        installed_version,
        latest_compatible: latest_compatible.map(|(_, v)| v.to_string()),
        latest: latest.map(|(_, v)| v.to_string()),
        incompatible_reason,
        note,
    }
}

/// Status of every tool in the catalog.
pub fn evaluate(
    catalog: &Catalog,
    paths: &Paths,
    platform: &Platform,
    channel: Channel,
) -> Vec<ToolView> {
    let kim = semver::Version::parse(crate::KIM_VERSION).expect("Kim's own version is SemVer");
    catalog
        .tools
        .iter()
        .map(|t| evaluate_tool(t, platform, &kim, &installed(paths, &t.id), channel))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Catalog {
        super::super::parse(include_bytes!(
            "../../tests/fixtures/catalog-hypothetical.json"
        ))
        .expect("fixture is valid")
    }

    fn win(build: u32) -> Platform {
        Platform {
            os: "windows".into(),
            arch: "x86_64".into(),
            os_build: Some(build),
        }
    }

    fn view(id: &str, platform: &Platform, installed: &Installed, channel: Channel) -> ToolView {
        let c = fixture();
        let kim = semver::Version::parse("0.1.0").unwrap();
        evaluate_tool(c.tool(id).unwrap(), platform, &kim, installed, channel)
    }

    fn v(s: &str) -> Installed {
        Installed::Version(semver::Version::parse(s).unwrap())
    }

    #[test]
    fn real_tool_subak_is_available_on_windows_x64_and_never_installed_by_evaluation() {
        let s = view("subak", &win(19045), &Installed::No, Channel::Preview);
        assert_eq!(s.status, ToolStatus::Available);
        assert_eq!(s.latest_compatible.as_deref(), Some("0.2.0"));
        // Stable channel only: subak has only preview releases.
        let s = view("subak", &win(19045), &Installed::No, Channel::Stable);
        assert_eq!(s.status, ToolStatus::Unreleased);
        assert!(s.note.unwrap().contains("--channel preview"));
    }

    #[test]
    fn installed_and_update_available() {
        let s = view("subak", &win(19045), &v("0.1.0"), Channel::Preview);
        assert_eq!(s.status, ToolStatus::UpdateAvailable);
        let s = view("subak", &win(19045), &v("0.2.0"), Channel::Preview);
        assert_eq!(s.status, ToolStatus::Installed);
        let s = view(
            "subak",
            &win(19045),
            &Installed::Damaged("x".into()),
            Channel::Preview,
        );
        assert_eq!(s.status, ToolStatus::Damaged);
    }

    #[test]
    fn os_build_too_old_is_incompatible_with_reason() {
        let s = view("subak", &win(17134), &Installed::No, Channel::Preview);
        assert_eq!(s.status, ToolStatus::Incompatible);
        assert!(s.incompatible_reason.unwrap().contains("build 17763"));
    }

    #[test]
    fn hypothetical_entries_cover_every_status() {
        let p = win(19045);
        let linux_only = view("example-linux-only", &p, &Installed::No, Channel::Stable);
        assert_eq!(linux_only.status, ToolStatus::Incompatible);
        assert!(
            linux_only
                .incompatible_reason
                .unwrap()
                .contains("no package for windows/x86_64")
        );
        let arm = view("example-arm-only", &p, &Installed::No, Channel::Stable);
        assert_eq!(arm.status, ToolStatus::Incompatible);
        let future = view("example-needs-new-kim", &p, &Installed::No, Channel::Stable);
        assert_eq!(future.status, ToolStatus::Incompatible);
        assert!(
            future
                .incompatible_reason
                .unwrap()
                .contains("requires Kim 9.0.0")
        );
        let stable = view("example-stable", &p, &Installed::No, Channel::Stable);
        assert_eq!(stable.status, ToolStatus::Available);
        assert_eq!(stable.latest.as_deref(), Some("1.2.0"));
        let deprecated = view("example-deprecated", &p, &v("1.0.0"), Channel::Stable);
        assert_eq!(
            (deprecated.support, deprecated.status),
            (Support::Deprecated, ToolStatus::Installed)
        );
        let unreleased = view("example-unreleased", &p, &Installed::No, Channel::Preview);
        assert_eq!(unreleased.status, ToolStatus::Unreleased);
    }

    #[test]
    fn newest_compatible_is_chosen_when_newest_overall_is_incompatible() {
        // example-stable 1.3.0 needs a newer Windows build than 19045; 1.2.0 does not.
        let s = view("example-stable", &win(19045), &v("1.0.0"), Channel::Preview);
        assert_eq!(s.latest.as_deref(), Some("1.3.0-beta.1"));
        assert_eq!(s.latest_compatible.as_deref(), Some("1.2.0"));
        assert_eq!(s.status, ToolStatus::UpdateAvailable);
    }

    /// KIM-FR-051: availability comes from the releases alone, never from a declaration.
    #[test]
    fn availability_is_derived_from_releases_and_shown_with_the_status() {
        let p = win(19045);
        let c = fixture();
        let a = |id: &str| Availability::of(c.tool(id).unwrap());
        assert_eq!(a("example-unreleased"), Availability::Planned);
        assert_eq!(a("subak"), Availability::Preview, "preview releases only");
        assert_eq!(a("example-stable"), Availability::Available);

        let planned = view("example-unreleased", &p, &Installed::No, Channel::Preview);
        assert_eq!(
            (planned.availability, planned.availability_label()),
            (Availability::Planned, Some("planned"))
        );
        let stable_only = view("subak", &p, &Installed::No, Channel::Stable);
        assert_eq!(stable_only.availability_label(), Some("preview only"));
        let preview = view("subak", &p, &Installed::No, Channel::Preview);
        assert_eq!(preview.availability_label(), Some("available (preview)"));
        let stable = view("example-stable", &p, &Installed::No, Channel::Stable);
        assert_eq!(stable.availability_label(), Some("available"));
        let installed = view("example-stable", &p, &v("1.2.0"), Channel::Stable);
        assert_eq!(installed.availability_label(), None);
        assert_eq!(
            serde_json::to_value(planned).unwrap()["availability"],
            "planned"
        );
    }

    #[test]
    fn install_records_are_read_without_side_effects() {
        let root = std::env::temp_dir().join(format!("kim-status-{}", std::process::id()));
        let paths = Paths::under(&root);
        assert_eq!(installed(&paths, "subak"), Installed::No);
        std::fs::create_dir_all(paths.install_record("subak").parent().unwrap()).unwrap();
        std::fs::write(paths.install_record("subak"), br#"{"version":"0.1.0"}"#).unwrap();
        assert_eq!(installed(&paths, "subak"), v("0.1.0"));
        std::fs::write(paths.install_record("subak"), b"garbage").unwrap();
        assert!(matches!(installed(&paths, "subak"), Installed::Damaged(_)));
        assert!(matches!(installed(&paths, "kim"), Installed::Version(_)));
        let _ = std::fs::remove_dir_all(root);
    }
}
