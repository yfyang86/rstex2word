# Publishing tex2word to crates.io — step by step

This is the operator runbook for the 1.0.6 release. The bug fixes, manifest
changes, and CI in this branch have already done the code-side work; what's
left is the manual account/registry steps and the one design decision below.

## What's already done in this branch

- **All eight manifests** now declare inter-crate deps with a `version` (via
  `[workspace.dependencies]` in the root `Cargo.toml`) — `cargo publish` no
  longer rejects them — and inherit `repository`/`homepage`/`keywords`/
  `categories`/`readme` so the crates.io pages are populated.
- **Verified bug fixes** for every reproduced crash, corruption, and DoS from
  the review (see `05-fixes-applied.md`), each with a regression test.
- **rustdoc warnings** that would show on docs.rs are cleared (only the
  harmless bin/lib name-collision note remains).
- **CI** (`.github/workflows/ci.yml`) gates `main` on
  fmt + clippy + test across stable and the MSRV (1.82).
- **Publish workflow** (`.github/workflows/publish.yml`) publishes all crates
  in dependency order when a `v*` tag is pushed.

## One decision to make before the first publish

**`#[non_exhaustive]` on the IR types** (`tex2word-ir`). This was *not* applied,
because it is a one-way design choice with a real trade-off, and it's yours to
make:

- The IR is documented as still growing. Once published *without*
  `#[non_exhaustive]`, adding any enum variant or struct field to `Inline`,
  `Block`, `Document`, `Table`, … is a **semver-major** break for downstream
  users.
- Adding `#[non_exhaustive]` prevents that, but it also forces the workspace's
  own `match`es on those enums (in `tex2word-frontend`/`-backend`/`-latex`/
  `crossref`) to grow a `_ =>` arm — which **silences the compiler's
  "you didn't handle the new variant" check** that this port currently relies
  on to stay complete.

Recommendation: since the port is still filling in coverage, keep the
exhaustiveness checking (no `#[non_exhaustive]`) *for now*, and treat the next
IR-growth release as `2.0.0`. If instead you want a stable 1.x IR that external
tools can depend on, add `#[non_exhaustive]` to the growing enums/structs
before this first publish and accept the internal `_` arms. Either way, decide
now — you cannot retrofit it without a major bump.

## Manual steps (one-time account setup)

1. **crates.io account + token.** Log in at <https://crates.io> with GitHub,
   then Account Settings → API Tokens → *New Token* with the **publish-new** and
   **publish-update** scopes. Copy the token (shown once).
2. **Add the token as a repo secret** for the publish workflow:
   GitHub repo → Settings → Secrets and variables → Actions → *New repository
   secret*, name `CARGO_REGISTRY_TOKEN`, paste the token.
3. **Verify crate-name availability** (all eight were free as of 2026-07-23):
   `tex2word`, `tex2word-ir`, `tex2word-math`, `tex2word-frontend`,
   `tex2word-backend`, `tex2word-latex`, `tex2word-validate`, `tex2word-cli`.
   Names are first-come; publishing `tex2word-ir` first also reserves it.

## Releasing (the automated path)

Once the branch is merged to `main` and CI is green:

```bash
git checkout main
git pull
git tag v1.0.6            # tag must match the workspace version
git push origin v1.0.6    # this fires .github/workflows/publish.yml
```

The workflow re-runs fmt/clippy/tests, then publishes the eight crates
leaf-first. Watch it under the repo's Actions tab.

## Releasing (the manual path, if you'd rather not use the workflow)

From a clean checkout of the tagged commit, with `cargo login <token>` done:

```bash
# Leaf crates first, then dependents. Modern cargo waits for the index to
# update between publishes, so this order just works.
cargo publish -p tex2word-ir
cargo publish -p tex2word-math
cargo publish -p tex2word-frontend
cargo publish -p tex2word-backend
cargo publish -p tex2word-latex
cargo publish -p tex2word-validate
cargo publish -p tex2word
cargo publish -p tex2word-cli
```

Dry-run any single crate first with `cargo publish -p <crate> --dry-run`
(note: a `--dry-run` of a dependent crate will fail until its dependencies are
actually on crates.io — that's expected, not a manifest error).

## Naming note: `cargo install tex2word`

The binary lives in `tex2word-cli`, so users install it with:

```bash
cargo install tex2word-cli
```

`cargo install tex2word` installs the *library* crate and produces no binary.
If you'd prefer `cargo install tex2word` to work (most people will try that
first), move the `[[bin]]` into the `tex2word` crate before publishing and drop
or thin out `tex2word-cli`. This is also a first-publish decision — changing it
later means yanking and renaming.

## Bumping versions later

The workspace shares one version (`[workspace.package].version`). To release a
new version, bump that single field (and the `version` in each
`[workspace.dependencies]` entry in the root `Cargo.toml`), update
`CHANGELOG.md`, commit, then tag `vX.Y.Z` and push the tag.

## Keeping CoderReview off main

This `CoderReview/` folder is a dev-branch artifact and is now in `.gitignore`.
It should reach the `dev` PR but **not** `main`. When promoting `dev` → `main`,
drop it:

```bash
# on the release branch, before merging to main
git rm -r --cached CoderReview
git commit -m "Drop dev-only CoderReview notes before release"
```

(Because it's git-ignored, it won't be re-added by accident.)
