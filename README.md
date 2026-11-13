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
| WebP | Encoded again as lossy WebP at the quality, with transparency kept. An animated WebP keeps its loop count and total duration, but not always each frame's timing: a frame that (nearly) matches the one before it is merged into it, and the merged frame lasts as long as both did. |
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
  gives `optimized-logo.png`. The app never replaces an output: when
  `optimized-logo.png` is already there, the new one is `optimized-logo-1.png`,
  then `optimized-logo-2.png`, and so on.
- **In an output folder you choose:** each file keeps its own name. A name that
  is already taken gets a number before the extension (`logo.png`, `logo-1.png`,
  `logo-2.png`, …), so nothing in that folder is ever overwritten. Choosing the
  originals' own folder is safe for the same reason.
- The command line without `--out` is the one place that still replaces: it
  overwrites an earlier `optimized-<name>` (with `--dir` and no `--out`, the
  one in the `optimized/` folder). With `--out` it numbers like the app.
- A file that is already as small as it gets is copied as it is, so the output
  is never larger than the input.
- Single image mode's Save writes to the folder you chose or, with none, next
  to the original, under the name you type. It never replaces a file and never
  numbers a name you typed (see "The window").

One run handles at most 2000 files, and each file has its own limits (100 MiB,
100 megapixels, 10 minutes).

## The window

The window has two modes, chosen with the switch at the top: **Single image**
and **Batch**. It opens in Single image mode every time. Single image mode
compresses one picture and shows the result before anything is written. Batch
mode runs a list of files in one go, as before.

### Single image mode

Use it to see what a setting does to one picture, and to save the result
under a name you choose.

1. Drop one image on the window, or press "Choose image" (the file picker
   picks one file). It can be a PNG, JPG, SVG or WebP. "Choose other" picks
   another one later. The picture is copied into a private folder when it is
   loaded and the original is never touched; the copy is removed when the
   window closes. Closing the window while a picture is still loading can
   leave that folder in your temp folder; the system's own clean-up of temp
   files removes it later. The same limits as in Batch apply (100 MiB, 100
   megapixels, 10 minutes).
2. The picture shows the original and the compressed version in one frame,
   split by a divider: the original on the left, the compressed one on the
   right. Drag the handle, or tab to it and use the arrow keys. A new picture
   starts shrunk to fit the window ("Fit"); a small picture is not enlarged
   to fill it. The zoom controls float at the bottom of the picture: "−" and
   "+", the zoom as a percentage, "100%" and "Fit". "−", "+", the mouse wheel
   and "100%" zoom up to 800% (an SVG up to eight times its fitted size);
   "Fit" goes back. Drag the picture to move it, or tab to it and use the
   arrow keys; a press or the wheel on the zoom controls leaves the picture
   alone. Both sides always show the same spot at the same zoom.
3. Choose **Lossless** or **Lossy**, and the quality, as in Batch; "Save to"
   works the same way too. The compressed side updates a moment after you let
   go of the slider (about 0.4 seconds after the last key when you type a
   number), not while you drag. "Compressing…" shows meanwhile, then
   "Loading picture…" while the window draws it, and the old picture stays
   until the new one shows; Save waits for it, so it always saves the version
   you see. A box above Save shows the original size, the compressed
   size and how much it saves for the version that shows, with a note
   when the file was handled differently from the setting (for example
   "original kept: lossy was not smaller").
4. "Save as" starts with the name Batch would give the file
   (`optimized-holiday` next to the original, `holiday` in a chosen folder),
   or the first free numbered form of it. Change it if you like; the
   original's extension is added. Press **Save**. Save never replaces a file
   and never numbers a name you typed: when the name is taken, it says so and
   writes nothing, and you choose another. After a save the box offers the
   next free name (`optimized-holiday-1`), so a second save works. A name may
   not contain `/ \ : * ? " < > |` or control characters, may not end in a
   space or a dot, may not be a name Windows reserves (`CON`, `NUL`, `COM1`,
   and so on), and is at most 200 characters and 255 bytes. When the
   original's folder cannot be written to, Save says so; choose a folder under
   "Save to" then. "Open folder" opens the folder of the last save.
5. Drop several files, or a folder, and the first image is loaded. A note says
   how many were left out, and **Switch to Batch** puts all the dropped files
   in the Batch list. A dropped folder loads nothing ("Folders go in Batch.")
   and offers the same button. The offer ends at the next picture you load
   and when you change mode by hand.

The picture fills the window under the header, and the settings sit in a
column on its right: the file's name and size with "Choose other", the
settings and "Save as" (this part scrolls when it does not fit), and at the
bottom the sizes, the status line, "Open folder" and **Save**. Before a
picture is loaded the column shows only the settings. **Hide settings** at
the top of the column folds it to a narrow strip, so the picture gets the
whole width; **Show settings** brings it back. While it is folded, the
status line (with "Switch to Batch" when it is offered) floats over the top
of the picture, or of the drop area, so a note is still seen and read out;
it shows only when there is something to say. The column starts shown at
every launch. In a window narrower than about 640 pixels (at the normal text
size) the picture comes first and the column sits under it, and the page
scrolls. Changing mode and coming back leaves the picture, the settings and
the preview as they were. The switch cannot be used while a Batch run or a
save is going.

### Batch mode

1. Drop images on the window, or press "Choose files". A dropped folder adds
   the PNG, JPG, SVG and WebP files directly inside it (not its subfolders).
   Adding files does not start anything: they appear in a list, each with a
   small picture before its name (see "Thumbnails" below), and a file that
   cannot be compressed is listed with "Will be skipped" and its reason.
2. Choose **Lossless** or **Lossy**. The quality appears when Lossy is
   picked: set it with the slider or type a whole number from 0 to 100 (it
   starts at 75). Under "Save to", leave "Next to the originals" or press
   "Choose folder"; "Clear" then goes back to next to the originals.
3. Press **Run**. Each file shows "Waiting", "Compressing…" and then its
   result: sizes, saving, time, and a note where a file was kept as it was. The
   heading shows the progress, for example "Compressing 2 of 3 · Lossy 75".
4. When it is done the heading says "Done · Lossy 75", with the total saved
   and two buttons: "Open folder", and **Re-add**.
   - **Re-add** puts the files of that run back in the list, with the
     settings as you left them, so the same files can run again with another
     setting: lossless, then Re-add, then Lossy 60, then Run. The first run's
     files stay as they were, and the second run's get a number
     (`optimized-photo.jpg`, then `optimized-photo-1.jpg`). Re-add brings back
     the files you added, not the `optimized-` files written next to them.
     Drop or pick more files after Re-add and they join the list.
   - Without Re-add, the next drop or pick starts a new list.
   - "Clear" empties the list, before or after a run.

Nothing is remembered between launches. Every start is in Single image mode,
Lossless, quality 75, next to the originals, with the settings column shown.

### Thumbnails

Every row of the Batch list starts with a small picture of the file, 32
pixels square (made from a 64 pixel image, so it stays sharp on a
high-resolution screen). It
shows for a PNG, JPG, WebP and SVG; an animated PNG or WebP shows its first
picture, and a photo that carries a rotation is shown the right way up. A
row that is skipped, or whose picture cannot be made (a damaged file, a CMYK
JPG, more than 100 megapixels, an SVG over 2 MiB), shows an empty square;
nothing is reported for it.

The window never opens your files itself: the same small worker program that
optimises them makes each picture, two at a time and at most 10 seconds each,
so a list of 2000 files appears at once and the pictures fill in as the rows
come into view. The pictures are kept in memory only, up to 64 MiB per list
(a picture that does not fit is made again when it is needed), and are gone
when the list is cleared or a new list starts. The app writes nothing to
disk for them.

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

The window script tests need Node 21 or later (one of them reads numbers
with `JSON.parse`'s source text); the Windows check runs them on Node 22.

The `--config` file adds the small worker program that optimises each file,
makes each thumbnail and reads the header of each image Single image mode
loads, and builds it first; without it the app still works, but starts each
file about ten times slower.

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
