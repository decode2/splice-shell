# Releases & Auto-Updates

Splice Shell distributes Windows installers through GitHub Releases and updates itself silently in the background.

## How a release is made

Every push to `master` is evaluated by [release-please](https://github.com/googleapis/release-please). When enough conventional commits accumulate to justify a new version, release-please opens a PR titled **"chore: release X.Y.Z"**. Merging that PR:

1. Bumps the version in `tauri.conf.json` and `Cargo.toml`.
2. Creates the tag `vX.Y.Z`.
3. Triggers the `release.yml` workflow, which compiles the Windows `.msi`, signs it, and uploads it to a GitHub Release draft.

Publish the draft when ready. That's the entire flow.

## Versioning — SemVer via Conventional Commits

| Commit type | Version bump |
|-------------|-------------|
| `fix:` | PATCH (`0.1.1`) |
| `feat:` | MINOR (`0.2.0`) |
| `feat!:` or `BREAKING CHANGE:` footer | MAJOR (`1.0.0`) |

release-please reads your commit history and picks the correct bump automatically.

## Quick path: ship a release

```powershell
# 1. Work on feature / fix branches as usual, merge to master with conventional commits.
# 2. release-please opens a "chore: release X.Y.Z" PR automatically.
# 3. Review and merge that PR.
# 4. The release.yml workflow builds and signs the .msi.
# 5. Publish the GitHub Release draft when ready.
```

No manual version edits, no manual tagging.

## Retry a release build

GitHub does not start a second workflow when `release-please` creates a tag
with the repository `GITHUB_TOKEN`. If a release exists without installer
assets, dispatch the release workflow explicitly for that existing tag:

```powershell
gh workflow run release.yml --ref master -f tag=v0.2.0
```

The workflow checks out and verifies the requested tag, then uploads the
signed installer and updater artifacts to its GitHub Release. Replace
`v0.2.0` with the release tag that needs to be rebuilt.

## Auto-updater

Installed copies of Splice Shell check for updates silently every time they start.

| Behaviour | Detail |
|-----------|--------|
| Check endpoint | `https://github.com/decode2/splice-shell/releases/latest/download/latest.json` |
| On update found | Downloads and installs in the background, then relaunches the app |
| On error (offline, bad signature, disk full) | Logs the failure and continues with the current version |
| User interaction required | None |

The updater verifies the cryptographic signature of every installer before applying it. A release without a valid `.sig` file will be rejected.

## One-time signing setup (maintainers only)

This was already done for this repository. Documented here for reference.

```powershell
# Generate the keypair
npx tauri signer generate -w ~/.tauri/splice-shell.key

# Add as GitHub repository secrets (Settings → Secrets → Actions):
#   TAURI_SIGNING_PRIVATE_KEY  →  contents of ~/.tauri/splice-shell.key
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD  →  the password chosen above

# The public key is already committed in tauri.conf.json under plugins.updater.pubkey
```

> **Important:** The private key and its password are the only thing that lets the updater trust a release. Keep them safe. If lost, existing installations cannot be updated automatically — users would need to reinstall manually.

## Files involved

| File | Role |
|------|------|
| `.github/workflows/release-please.yml` | Runs release-please on every push to `master` |
| `.github/workflows/release.yml` | Builds, signs, and uploads the `.msi` on tag push |
| `release-please-config.json` | Tells release-please which files to bump |
| `.release-please-manifest.json` | Tracks the current version (managed automatically) |
| `apps/desktop/src-tauri/tauri.conf.json` | Version field and updater endpoint/pubkey |
| `apps/desktop/src-tauri/src/lib.rs` | Background update check logic at startup |
