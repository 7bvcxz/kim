# Security policy

## Supported versions

Kim and Subak are **pre-alpha**. No version is supported for production use yet, and there are
no released binaries. Security fixes land on `main`. This section will list supported release
lines once `docs/RELEASE_POLICY.md` (Step 06) defines them.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Report privately through GitHub: open the repository's **Security** tab →
**Report a vulnerability** (GitHub private vulnerability reporting). Include:

- affected component (Kim manager, Subak Host, Subak CLI/GUI, installer, catalog, docs);
- version or commit, Windows version and edition;
- reproduction steps or proof of concept, and the impact you observed;
- whether the issue is already public.

What to expect (best effort, maintained by one person):

- acknowledgement within **7 days**;
- an initial assessment and planned fix window within **14 days**;
- coordinated disclosure: we publish an advisory once a fix is available, crediting you unless
  you prefer otherwise.

### In scope

Anything that breaks a promise in [docs/PRODUCT_REQUIREMENTS.md](docs/PRODUCT_REQUIREMENTS.md),
for example: installing or updating without explicit consent (`KIM-SEC-001`), skipped or
bypassable integrity/signature checks (`KIM-SEC-003`), silent elevation (`KIM-SEC-004`), secrets
in logs/JSON/state (`KIM-SEC-013`), another local user or process controlling or reading a Subak
session (`SUBAK-SEC-010`, `SUBAK-SEC-012`), destructive session recovery (`SUBAK-SEC-001…003`),
unexpected network activity (`KIM-NFR-004`).

### Out of scope

Attacks that require an already-compromised administrator account or physical access to an
unlocked session, and findings in third-party software (report those upstream; tell us if Kim
should mitigate them).

## Dependency and supply-chain policy

Every dependency is code we ship. The policy:

1. **Minimal dependencies.** Prefer the standard library and small, well-maintained crates.
   A new dependency needs a reason in the PR description (what it replaces, maintenance status,
   license, transitive weight). Tools must not pull in another tool's UI runtime.
2. **Pinned and reproducible.** `Cargo.lock` is committed and CI builds with `--locked`. The
   Rust toolchain is pinned in `rust-toolchain.toml`.
3. **Automated checks on every push and PR** with [cargo-deny](https://github.com/EmbarkStudios/cargo-deny)
   (`deny.toml`):
   - **advisories** — RustSec advisory database; known-vulnerable or yanked crates fail CI;
   - **licenses** — only permissive licenses compatible with `MIT OR Apache-2.0` are allowed;
     anything else needs a reviewed exception;
   - **sources** — crates.io only; no git or unknown registries;
   - **bans** — wildcard version requirements are rejected; duplicate versions are reported.
4. **Updates are deliberate.** Dependency updates are made manually in reviewed PRs (never
   auto-merged). An ignored advisory must reference an issue and say why it is safe.
5. **No network in builds.** Build scripts must not download anything.
6. **Release artifacts** (from Step 06/40) are hashed and signed; users verify before install
   (`KIM-SEC-003`).

Run the same checks locally with `cargo deny check` (install with `cargo install cargo-deny`).
