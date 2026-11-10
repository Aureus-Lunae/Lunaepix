# Lunaepix roadmap

What needs to be done. This list is not an order: which item comes next is
decided when the previous one is finished. Each item gets its own spec and
plan (in `docs/pending-approval/` until approved) when it starts.

| Item | Status |
|---|---|
| Image lossless compression | Done |
| Lossy image compression | Built; being tested by hand |
| Preview single image compression before compressing (before/after slider) | Not started |
| Apple support: a brew formula, and a plain download with one awkward step | Not started |
| Choose how much it can get compressed (quality, like JPEG 70%) on lossy | Part of lossy compression |
| Export rename (single file only) | Not started |
| Clean and clear UI | Not started |
| Automated draft release | Not started |

## Image lossless compression — Done

"Done, might need a bit of looks design but functionality works."

- Desktop app 0.1.0 for Linux (deb, rpm) and Windows (installer): PNG and JPG
  made smaller without changing a pixel.
- WebP (branch `feature/lunaepix-webp`, tag `v0.1.0-rc5`): existing `.webp`
  files are made smaller by removing metadata. EXIF is kept when the picture
  is rotated, and the ICC profile is always kept. Lossless and animated WebP
  files are copied unchanged.
- Being tested by hand now. Then `/finalize-plan` closes the WebP plan.
- The looks are picked up under "Clean and clear UI".

## Lossy image compression

Smaller files by allowing a change in pixels that the eye should not notice.
Built together with the quality setting ("Why not. Make them at once."), plus
a Run button, an output folder and the same options on the command line.
Spec: `docs/in-progress/specs/2026-11-09-lunaepix-lossy-design.md`.
Plan: `docs/in-progress/plans/2026-11-09-lunaepix-lossy.md`.

## Preview single image compression before compressing

A before/after slider on one image, so the result can be judged before the
files are written. Needs lossy compression for there to be a visible
difference; most useful together with the quality setting.

## Apple support

Two routes, both free of the $99-a-year Apple developer fee:

- **Brew formula** that builds from source on the user's Mac. Programs built
  locally are not quarantined, so Gatekeeper does not block them. Costs the
  user a Rust toolchain and a long first build.
- **Plain download** (`.dmg` from the GitHub release), unsigned. The one
  awkward step: on first launch, System Settings › Privacy & Security ›
  "Open Anyway" (right-click › Open no longer works since macOS Sequoia).
  The README explains it.

Known before starting:

- Homebrew's own cask repository no longer accepts unsigned apps (disabled
  since 2026-09-01) and dropped `--no-quarantine`, so a cask is not a route.
- Apple Silicon needs an ad-hoc signature; the Rust/Tauri build adds one.
- GitHub Actions has macOS runners, so the download can be built in CI next
  to Linux and Windows. Hand testing needs a Mac.

## Choose the quality on lossy

A setting for how far lossy compression may go, like JPEG 70%. Delivered as
part of lossy compression (quality 0–100, starting at 75).

## Export rename (single file only)

Give the output file a different name when one file is compressed.

## Clean and clear UI

The design pass over the whole app, including the looks noted under
lossless compression.

## Automated draft release

On a `v*` tag, the workflow creates a draft GitHub Release for that tag with
the installers attached; publishing stays a click by hand. Artifacts of a run
expire after 30 days, release files do not.

Known before starting:

- Only the release job gets `contents: write`; every other job stays
  read-only, and `src-tauri/tests/workflow.rs` is updated to check exactly
  that.
- Linux packages are built locally today. Either a Linux build job is added,
  or the `.deb` and `.rpm` are attached to the draft by hand.
- The repository is private, so release files only download for people with
  access. A public download (and the brew formula, which needs a public URL)
  needs the repository made public or a separate public release repository.
