<img src="assets/icon.svg" width="128" height="128" alt="ArkImage icon">

# ArkImage

A fast, light image viewer for Windows and macOS, written in Rust.

ArkImage brings together the best ideas of several image viewers, adds some of its own,
and puts them in one small program.

## Download

Get it from the [latest release](https://github.com/exodus941/ArkImage/releases/latest).

### Windows

Download `arkimage.exe`. It is a single file with nothing to unpack.

- **Portable:** put it in any folder and run it. It keeps its settings in `ArkImage.ini`
  beside itself.
- **Installed:** on first run, ArkImage offers to install itself for your user, with no
  administrator rights. It copies itself to `AppData\Local\Programs\ArkImage`, adds Start
  menu and desktop shortcuts, and registers for every image type it opens. Remove it from
  Settings > Apps.
- **Settings after an install:** the install moves `ArkImage.ini` to
  `AppData\Roaming\ArkImage`. From then on, every copy uses those settings, portable
  copies too.

### macOS

Download `ArkImage-<version>-macOS.dmg` and drag ArkImage to Applications. It needs
macOS 11 or later on Apple Silicon.

- The app is not signed with an Apple Developer ID. The first time you open it, macOS
  blocks it: open System Settings > Privacy & Security and click **Open Anyway**.
- To open images with ArkImage by default, click **Make Default** in Settings.

## Features

- Opens JPEG, PNG, GIF and APNG, WebP, BMP, TIFF, TGA, DDS, ICO, PNM, HDR, EXR, QOI,
  PSD and PSB, HEIC/HEIF, SVG/SVGZ, and Lottie animations (`.lottie` and Lottie `.json`).
- Plays GIF, APNG, WebP and Lottie animations with a scrub bar, a frame counter and
  playback speeds from 0.25× to 4×.
- Reads images straight from ZIP/CBZ, RAR/CBR and 7z/CB7 archives, including archives inside archives.
- One process serves every window: images open in a shared cache and are never decoded twice.
- Preloads the next images in the direction you browse.
- Thumbnail grid with a disk cache.
- Filter presets: resize kernels (bilinear to Lanczos4 and Mitchell) and an adjustable sharpen.
- Zoom to a rectangle: drag over the image, or hold the right button and drag when zoomed in.
- Touchpad: pinch to zoom, swipe sideways with two fingers for the previous or next image.
- Double-click the image at fit or 100% to go full screen.
- Remembers each window's size, position, monitor and state.
- Recovers on its own: a backup process reopens your windows if the app crashes or hangs.
- Updates itself from GitHub Releases and reopens your windows after the update.
- Custom hotkeys and mouse buttons. Press F1 for the full list, F5 for Settings.

## Build (Windows)

You need Rust and, for HEIC and Lottie, libheif and rlottie from
[vcpkg](https://github.com/microsoft/vcpkg), linked statically. Install them once, point
`VCPKG_ROOT` at your vcpkg folder, then build:

```bash
vcpkg install "libheif[core,libde265]:x64-windows-static-md" rlottie:x64-windows-static-md
```

```bash
cargo build --release
```

The result is `target/release/arkimage.exe`, with no DLLs beside it.
`.cargo/config.toml` selects the static libraries.

For a release, build with `scripts/build-release.ps1`. It maps the build machine's folder
paths out of the exe, so the file names no user or folder of the machine that built it.

Test fixtures and tests:

```bash
cargo run --example make_fixtures
```

```bash
cargo test
```

## License

MIT; see [LICENSE](LICENSE).

libheif and libde265 are LGPL-3.0 and linked statically. ArkImage's source is open, so you
can rebuild it against other versions of them. rlottie is MIT.

## Credits

- [Geist and Geist Mono](https://github.com/vercel/geist-font) by Vercel, v1.9.2, SIL Open Font License 1.1 (`assets/fonts/OFL-Geist.txt`).
- [Figtree](https://github.com/erikdkennedy/figtree) by Erik Kennedy, SIL Open Font License 1.1 (`assets/fonts/OFL-Figtree.txt`).
- [Tabler Icons](https://github.com/tabler/tabler-icons) by Paweł Kuna, v3.52.0, MIT License (`assets/icons/LICENSE-tabler.txt`).
- [rlottie](https://github.com/Samsung/rlottie) by Samsung, MIT License.
