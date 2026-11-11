# LightCode app icon

The app mark is `||{}`, an empty Rust closure. The approved design uses charcoal
(`#222624`) and warm white (`#F0EFEB`) with a shallow relief.

## macOS 26 and later

The canonical artwork is
`crates/gram/resources/AppIcon.icon/Assets/mark.svg`. It contains only the flat
foreground shapes, on a full 1024 × 1024 canvas. The icon background, mask,
lighting, shadow, and Liquid Glass material belong to Icon Composer.

`AppIcon.icon/icon.json` defines two matching appearances:

- Default: warm-white background, charcoal mark.
- Dark: charcoal background, warm-white mark.

Both use the same vector artwork. The system selects and renders the appearance
according to the user's **Icon & widget style** setting in macOS. This selection
is independent of the editor theme. Clear and tinted treatments are supplied by
the system from the layered icon; there is no application-side appearance watcher.

`script/mac-icon-to-car` compiles the `.icon` package with Xcode 26 or later.
`script/bundle-mac` installs its `Assets.car` into the app resources and sets
`CFBundleIconName` to `AppIcon`, matching the compiled asset. Icon compilation
failures stop packaging. The existing `CFBundleIconFile` is retained for legacy
systems.

Use Icon Composer or `xcrun ictool` from a full Xcode installation to inspect the
native default, dark, clear, and tinted renderings. Static PNG exports are legacy
fallbacks and previews, rather than captures of the system's Liquid Glass renderer.

## Legacy and other platforms

`script/generate-app-icons` exports the same geometry into the 512px and 1024px
PNG files used by Cargo Bundle and into the Windows ICO files. It also exports
light PNGs and both SVG previews. The default legacy and development icon is
dark. The exported SVGs add a restrained edge highlight and contact shadow.

Install `librsvg` to provide `rsvg-convert`, then run:

```sh
./script/generate-app-icons
```

The generated files are committed alongside their vector source so builds do
not need an SVG converter. Native icon compilation still requires full Xcode.

References: [Apple Icon Composer](https://developer.apple.com/documentation/xcode/creating-your-app-icon-using-icon-composer)
and [CFBundleIconName](https://developer.apple.com/documentation/bundleresources/information-property-list/cfbundleiconname).
