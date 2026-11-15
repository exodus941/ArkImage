<img src="assets/icon.svg" width="128" height="128" alt="ArkImage icon">

# ArkImage

A fast, light image viewer for Windows, written in Rust.

ArkImage brings together the best ideas of several image viewers, adds some of its own,
and puts them in one small program. Run it from any folder as a portable app, or install
it for your Windows user with one click.

## Download

Get `arkimage.exe` from the [latest release](https://github.com/exodus941/ArkImage/releases/latest).
It is a single file with nothing to unpack.

- **Portable:** put it in any folder and run it. To keep its settings beside it, create an
  empty `ArkImage.ini` in the same folder.
- **Installed:** on first run, ArkImage offers to install itself for your user, with no
  administrator rights. It copies itself to `AppData\Local\Programs\ArkImage`, adds Start
  menu and desktop shortcuts, and registers for every image type it opens. Remove it from
  Settings > Apps.

## Features

- Opens JPEG, PNG, GIF and APNG, WebP, BMP, TIFF, TGA, DDS, ICO, PNM, HDR, EXR, QOI,
  PSD and PSB, HEIC/HEIF, and SVG/SVGZ.
- Reads images straight from ZIP/CBZ, RAR/CBR and 7z/CB7 archives, including archives inside archives.
- One process serves every window: images open in a shared cache and are never decoded twice.
- Preloads the next images in the direction you browse.
- Thumbnail grid with a disk cache.
- Filter presets: resize kernels (bilinear to Lanczos4 and Mitchell) and an adjustable sharpen.
- Zoom to a rectangle: drag over the image, or hold the right button and drag when zoomed in.
- Remembers each window's size, position, monitor and state.
- Recovers on its own: a backup process reopens your windows if the app crashes or hangs.
- Updates itself from GitHub Releases, in its own folder, installed or portable.
- Custom hotkeys and mouse buttons. Press F1 for the full list, F5 for Settings.

## Build

You need Rust and, for HEIC on Windows, libheif from [vcpkg](https://github.com/microsoft/vcpkg),
linked statically. Install libheif once, point `VCPKG_ROOT` at your vcpkg folder, then build:

```bash
vcpkg install "libheif[core,libde265]:x64-windows-static-md"
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
can rebuild it against other versions of them.

## Credits

- [Geist and Geist Mono](https://github.com/vercel/geist-font) by Vercel, v1.9.2, SIL Open Font License 1.1 (`assets/fonts/OFL-Geist.txt`).
- [Figtree](https://github.com/erikdkennedy/figtree) by Erik Kennedy, SIL Open Font License 1.1 (`assets/fonts/OFL-Figtree.txt`).
- [Tabler Icons](https://github.com/tabler/tabler-icons) by Paweł Kuna, v3.52.0, MIT License (`assets/icons/LICENSE-tabler.txt`).
