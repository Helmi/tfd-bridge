# Releasing TFD Bridge

## Versioning

TFD Bridge uses [SemVer](https://semver.org/): `MAJOR.MINOR.PATCH`.

- `PATCH` — backwards-compatible bug fixes or small additions.
- `MINOR` — new user-visible functionality, backwards-compatible.
- `MAJOR` — breaking changes (rare; the app has no public API, so this mainly covers major UX overhauls).

Current series is `0.x` (pre-1.0). Bump `MINOR` for meaningful feature releases, `PATCH` for fixes.

## Release flow

### 1. Update CHANGELOG.md

Move the `[Unreleased]` section to a new versioned heading:

```md
## [0.2.0] — YYYY-MM-DD
```

Add a new empty `[Unreleased]` section above it. Update the comparison links at the bottom of the file.

### 2. Draft and confirm release notes — HARD GATE

Write (or review) the notes for this version. They will appear verbatim as the GitHub Release body and in the in-app update prompt.

**Do NOT proceed past this point without explicit sign-off from Frank.**

Confirm:
- The CHANGELOG entry is accurate and complete.
- The release notes read well for a user seeing them in the update dialog.

### 3. Bump the version

Update the version string in both files to match the new release:

- `src-tauri/tauri.conf.json` — `"version"` field.
- `src-tauri/Cargo.toml` — `version` field in `[package]`.

Keep them in sync. The `crates/bridge-core/Cargo.toml` version is independent and only needs bumping if bridge-core's public interface changes.

### 4. Commit

```
git add CHANGELOG.md src-tauri/tauri.conf.json src-tauri/Cargo.toml
git commit -m "chore(release): v0.2.0 — <one-line summary>"
```

### 5. Tag and push

```
git tag v0.2.0
git push origin main --tags
```

The tag push triggers the `release.yml` CI workflow.

### 6. CI builds and publishes

The workflow:

1. Checks out the repo.
2. Extracts the matching `## [0.2.0]` section from `CHANGELOG.md` as the release body.
3. Builds the NSIS installer for Windows.
4. Signs the update artifact with the ed25519 key (from CI secrets).
5. Publishes a **full** (non-prerelease) GitHub Release with:
   - The installer `.exe`.
   - `latest.json` (updater manifest, signed).
   - The curated CHANGELOG notes + SmartScreen footnote as the release body.

The updater endpoint is `https://github.com/Helmi/tfd-bridge/releases/latest/download/latest.json`. GitHub's `/releases/latest/` only resolves to full (non-prerelease) releases — this is why `prerelease: false` is mandatory in the workflow.

### 7. Verify

- Open the GitHub Release page and confirm the notes look correct.
- Check that `latest.json` is present as a release asset.
- Optionally install the new build and confirm the in-app updater prompt appears on the previous version.

---

## SmartScreen "Run anyway"

Users installing the `.exe` on Windows will see a SmartScreen dialog: "Windows protected your PC — More info → Run anyway."

This is caused by the app being **unsigned for Authenticode** (no OS code-signing certificate). It has nothing to do with `prerelease` status on GitHub. The trust mitigation is open-source auditability: the source is public and the installer is built by reproducible CI.

The **Tauri updater ed25519 signature** is a separate, independent mechanism that covers artifact integrity for the auto-update flow. It does not affect SmartScreen.

---

## CI secrets required

| Secret | Purpose |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | ed25519 private key — signs update artifacts |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Passphrase for the private key |

The matching public key is baked into the app via `tauri.conf.json` (`plugins.updater.pubkey`). The private key is never committed to the repo.
