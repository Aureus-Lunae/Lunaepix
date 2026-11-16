class Lunaepix < Formula
  desc "Make PNG, JPG and WebP images smaller, lossless or lossy"
  homepage "https://github.com/Aureus-Lunae/Lunaepix"
  url "https://github.com/Aureus-Lunae/Lunaepix/archive/refs/tags/v0.1.5.tar.gz"
  # The real checksum is filled in by the commit right after the release
  # tag (docs/releasing.md); it cannot be in the tagged commit itself.
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "MIT"

  depends_on "rust" => :build
  depends_on arch: :arm64
  depends_on macos: :big_sur

  # Homebrew's clean-up may strip or rewrite Mach-O files after install;
  # any change inside the app breaks its seal.
  skip_clean "Lunaepix.app"

  def install
    system "cargo", "install", "--target-dir", "target", *std_cargo_args(path: "crates/cli")

    # Same version as both workflows (src-tauri/tests/formula.rs checks it),
    # inside the build folder, never in ~/.cargo.
    tauri = buildpath/"tauri-cli"
    system "cargo", "install", "tauri-cli", "--locked", "--version", "2.12.4", "--root", tauri
    ENV.prepend_path "PATH", tauri/"bin"

    # The Mac workflow's build, without the disk image: the worker is
    # staged and sealed in, and the app is signed ad hoc.
    system "cargo", "tauri", "build", "--bundles", "app",
           "--config", "src-tauri/tauri.bundle.conf.json"
    prefix.install "target/release/bundle/macos/Lunaepix.app"
  end

  def caveats
    <<~EOS
      Lunaepix.app is installed in:
        #{opt_prefix}/Lunaepix.app

      To open it from the Applications folder, link it there once. If Lunaepix
      from the download is in Applications, move it to the Bin first.
        ln -sfn "#{opt_prefix}/Lunaepix.app" /Applications/Lunaepix.app

      The link keeps working after `brew upgrade`. Spotlight and Launchpad
      do not list a linked app: open Lunaepix from the Applications folder.
    EOS
  end

  test do
    # A 1x1 PNG (RGBA, red), written here and compressed into out/.
    png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
    File.binwrite(testpath/"dot.png", png.unpack1("m"))
    system bin/"lunaepix", "--out", testpath/"out", testpath/"dot.png"
    assert_path_exists testpath/"out/dot.png"

    app = prefix/"Lunaepix.app"
    assert_predicate app/"Contents/MacOS/lunaepix-worker", :executable?
    system "codesign", "--verify", "--deep", "--strict", app
  end
end
