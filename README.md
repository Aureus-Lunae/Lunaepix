# Lunaepix

Lunaepix makes PNG, JPG, SVG and WebP images smaller. You choose how:

- **Lossless** (the default) keeps the picture exactly as it was: the pixels of
  a PNG or JPG stay exactly as they were, and an SVG keeps drawing the same
  picture (rounding of coordinates can move an anti-aliased edge by a hair). A
  WebP's image data is copied byte for byte; only its metadata (XMP, and EXIF
  unless it carries a rotation) and unused chunks are removed, so a WebP
  without metadata, like the ones the Jugoo CMS makes, is copied unchanged.
- **Lossy** allows a small change in pixels that the eye should not notice, for
  a much smaller file, and takes a **quality from 0 to 100** (75 to start
  with). A lower quality gives a smaller file and a bigger change. It applies
  to JPG, PNG and WebP (an animated WebP too; an animated PNG stays lossless).

It never touches the original.

There are two ways to use it, built from the same code:

- **The window app** (Tauri), described under "The window".
- **The command line**, `lunaepix`, described under "The command line".

## What lossy does

A lossy run handles each format like this. The result is a PNG, JPG or WebP
again; Lunaepix never converts between formats.

| Format | In a lossy run |
|---|---|
| JPG | Decoded and encoded again at the quality (mozjpeg). A photo's rotation and its colour profile are kept. |
| PNG | Reduced to a palette of at most 256 colours, with dithering and transparency kept. The quality sets how many colours: 256 at 100, fewer below. A transparent PNG stays transparent. |
| WebP | Encoded again as lossy WebP at the quality, with transparency kept. An animated WebP keeps its frame timing and loop count. |
| SVG | Has no lossy mode: it gets the lossless treatment. |

A few files are kept as they are, and the result says why:

- `kept lossless: animated PNG`: an animated PNG is never made lossy.
- `kept lossless: CMYK JPG`: a JPG that is not RGB or greyscale.
- `kept lossless: animation too large for lossy`: an animated WebP with too
  many frames and pixels to re-encode in reasonable time.
- `original kept: lossy was not smaller`: the lossy result was not smaller, so
  the original bytes are copied. A lossy output is never larger than the input.

A JPG that is already lossy loses a little more each time it goes through a
lossy run. Run lossy on your originals, not on earlier lossy outputs.

## Where the files go

- **Next to the original** (the default): `optimized-<name>`, so `logo.png`
  gives `optimized-logo.png`. An earlier `optimized-logo.png` is replaced.
- **In an output folder you choose:** each file keeps its own name. A name that
  is already taken gets a number before the extension (`logo.png`, `logo-2.png`,
  `logo-3.png`), so nothing in that folder is ever overwritten. Choosing the
  originals' own folder is safe for the same reason.
- A file that is already as small as it gets is copied as it is, so the output
  is never larger than the input.

One run handles at most 2000 files, and each file has its own limits (100 MiB,
100 megapixels, 10 minutes).

## The window

1. Drop images on the window, or press "Choose files". A dropped folder adds
   the PNG, JPG, SVG and WebP files directly inside it (not its subfolders).
   Adding files does not start anything: they appear in a list, and a file
   that cannot be compressed is listed with "Will be skipped" and its reason.
2. Choose **Lossless** or **Lossy**. With Lossy, set the quality with the
   slider or type a whole number from 0 to 100 (it starts at 75). Under "Save
   to", leave "Next to the originals" or press "Choose output folder".
3. Press **Run**. Each file shows "Waiting", "Compressing…" and then its
   result: sizes, saving, time, and a note where a file was kept as it was. The
   heading shows the progress, for example "Compressing 2 of 3 · Lossy 75".
4. When it is done the heading says "Done · Lossy 75", with the total saved
   and an "Open folder" button. The next drop or pick starts a new list.
   "Clear" empties the list before a run.

Nothing is remembered between launches: every start is Lossless, quality 75,
next to the originals.

## The command line

```
lunaepix [--lossy [--quality <0-100>]] [--out <folder>] [--] <file>...
lunaepix [--lossy [--quality <0-100>]] [--out <folder>] --dir <dir>
```

- Without `--lossy` the run is lossless, as it always was.
- `--lossy` makes it lossy. `--quality <0-100>` sets the quality (a whole
  number, 75 when left out) and needs `--lossy`.
- `--out <folder>` writes the results into that folder under the files' own
  names, with a number added when a name is taken (see "Where the files go").
  The folder is created when it is missing.
- `--dir <dir>` takes the files directly inside a folder. Without `--out`
  their results go to an `optimized/` folder inside it. `--` ends the options,
  so a file named `--dir` can be passed after it.

```
lunaepix photo.jpg                                  # lossless: optimized-photo.jpg
lunaepix --lossy photo.jpg                          # lossy at 75
lunaepix --lossy --quality 60 --out small a.jpg b.png   # lossy at 60 into ./small
lunaepix --dir pictures                             # lossless into pictures/optimized/
```

It prints one block per optimised file. A lossy run first prints
`Lossy, quality <q>`, and a file that was kept as it was has its note on the
line under its sizes:

```
Lossy, quality 75
anim.png:
Done in 380 ms!
500.000 KiB - 2.7% = 486.328 KiB
kept lossless: animated PNG

```

A skipped or failed file gets one line on stderr. The exit code is 0, or 1 if a
file failed (a skipped file is not a failure), or 2 if the arguments were
wrong. Build it with `cargo build --release -p lunaepix-cli`; the program is
`target/release/lunaepix`.

## Building on Linux

Tauri needs a few system packages (a WebKitGTK 4.1 and GTK 3 stack, among
others). Install what the Fedora section of
<https://tauri.app/start/prerequisites/> lists, plus Rust 1.90 or newer
(<https://rustup.rs>).

Lossy JPG uses mozjpeg's SIMD code, which the NASM assembler builds when the
program is compiled. Every build needs `nasm` on the path, the command line
program and the tests included:

```
sudo dnf install nasm
```

Then:

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
The workflow installs NASM itself.

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

WebP uses Google's libwebp (through the `libwebp-sys` crate): its code is
BSD-3-Clause, and Google's additional patent grant for it (`PATENTS`) is
printed in the same page. PNG colour reduction uses the `quantizr` crate (MIT).
`about.toml` clarifies both crates from their own licence files, with checksums.

`THIRD-PARTY-LICENSES.html` is generated from the dependencies the app ships
(not the test-only ones) and the rules in `about.toml`. Regenerate it after a
dependency changes, and read the failure if cargo-about refuses a licence:

```
cargo install --locked cargo-about --features cli    # once
cargo about generate -c about.toml about.hbs -o THIRD-PARTY-LICENSES.html
```
