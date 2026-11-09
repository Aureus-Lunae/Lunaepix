# Lunaepix

Lunaepix makes PNG, JPG, SVG and WebP images smaller without changing how they look.
It is lossless: the pixels of a PNG or JPG stay exactly as they were, and an
SVG keeps drawing the same picture (rounding of coordinates can move an
anti-aliased edge by a hair). It never touches the original.

- A file is written next to the original as `optimized-<name>`, so `logo.png`
  gives `optimized-logo.png`.
- A folder is handled one level deep (no subfolders); its files are written to
  an `optimized/` folder inside it, under their own names.
- A file that is already as small as it gets is copied as it is, so the output
  is never larger than the input.

There are two ways to use it, built from the same code:

- **The window app** (Tauri): drop files or a folder on the window, or use the
  "Choose files" and "Choose folder" buttons.
- **The command line**, `lunaepix`:

  ```
  lunaepix [--] <file>...
  lunaepix --dir <dir>
  ```

  It prints one block per optimised file and exits with 1 if a file failed, 2
  if the arguments were wrong. Build it with
  `cargo build --release -p lunaepix-cli`; the program is
  `target/release/lunaepix`.

## Building on Linux

Tauri needs a few system packages (a WebKitGTK 4.1 and GTK 3 stack, among
others). Install what the Fedora section of
<https://tauri.app/start/prerequisites/> lists, plus Rust 1.90 or newer
(<https://rustup.rs>). Then:

```
cargo install tauri-cli --locked --version 2.12.4             # once
cargo tauri build --config src-tauri/tauri.bundle.conf.json   # bundles end up in target/release/bundle/
cargo test --workspace                                        # the Rust tests
node --test "tests/ui/*.test.js"                              # the window script tests
```

The `--config` file adds the small worker program that optimises each file,
and builds it first; without it the app still works, but starts each file
about ten times slower.

`cargo tauri dev` runs the window app from the source without bundling.

## Getting the Windows installer

The installers are built on GitHub Actions; nothing here builds them locally.

1. In the repository on GitHub, open Actions, choose the "Windows build"
   workflow (`.github/workflows/windows.yml`) and press "Run workflow". Pushing
   a tag that starts with `v` (for example `v0.1.0`) starts it too.
2. When the run is green, download the `lunaepix-windows-installer` artifact
   from the run's page. It holds an NSIS `.exe` and an `.msi`.

The installers are **not signed**, so Windows SmartScreen warns when you start
one: choose "More info", then "Run anyway".

## Licence

Lunaepix is released under the MIT licence; see [LICENSE](LICENSE).

It includes third-party code, each part under its own licence (permissive
ones, plus MPL-2.0). The full list with the licence texts is in
[THIRD-PARTY-LICENSES.html](THIRD-PARTY-LICENSES.html), which is also
installed with the app. JPEG optimisation uses mozjpeg, which carries the
licence of the Independent JPEG Group:

This software is based in part on the work of the Independent JPEG Group.

`THIRD-PARTY-LICENSES.html` is generated from the dependencies the app ships
(not the test-only ones) and the rules in `about.toml`. Regenerate it after a
dependency changes, and read the failure if cargo-about refuses a licence:

```
cargo install --locked cargo-about --features cli    # once
cargo about generate -c about.toml about.hbs -o THIRD-PARTY-LICENSES.html
```
