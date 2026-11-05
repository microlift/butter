//! Mac application bundles: what their `Info.plist` says about them, their
//! icons, and the links and documents they open.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use gtk::prelude::*;
use plist::{Dictionary, Value};
use sha2::{Digest, Sha256};

use crate::config::Paths;

/// The largest icon Butter keeps. The Dynamic Launcher portal takes nothing
/// bigger.
pub const ICON_SIZE: u32 = 512;

/// An `.app` bundle and what it says about itself.
#[derive(Debug, Clone, Default)]
pub struct App {
    pub path: PathBuf,
    pub name: String,
    pub version: Option<String>,
    /// The bundle's `.icns` file.
    pub icon: Option<PathBuf>,
    /// `LSApplicationCategoryType`, such as `public.app-category.games`.
    pub category: Option<String>,
    /// URL schemes it opens, lowercased.
    pub schemes: Vec<String>,
    /// MIME types of the documents it opens.
    pub mime_types: Vec<String>,
    /// Whether it was found in a folder applications are installed in.
    pub installed: bool,
    /// The icon as a PNG, once [`App::render_icon`] has made it.
    pub icon_png: Option<PathBuf>,
}

impl App {
    /// Everything a desktop entry's `MimeType=` lists: the documents, then a
    /// `x-scheme-handler/` type for each scheme.
    pub fn handled_types(&self) -> Vec<String> {
        let mut types = self.mime_types.clone();
        types.extend(self.schemes.iter().map(|s| format!("x-scheme-handler/{s}")));
        types
    }

    /// The freedesktop categories its Mac category is closest to.
    pub fn categories(&self) -> &'static str {
        let category = self.category.as_deref().unwrap_or_default();
        let category = category
            .strip_prefix("public.app-category.")
            .unwrap_or(category);
        match category {
            c if c == "games" || c.ends_with("-games") => "Game;",
            "developer-tools" => "Development;",
            "graphics-design" | "photography" => "Graphics;",
            "music" | "video" | "entertainment" => "AudioVideo;",
            "productivity" | "business" | "finance" => "Office;",
            "education" | "reference" => "Education;",
            "social-networking" | "news" => "Network;",
            "utilities" => "Utility;",
            _ => "",
        }
    }

    /// The icon as a PNG of at most [`ICON_SIZE`] pixels, made from the
    /// bundle's `.icns` the first time it is asked for and cached after.
    pub fn render_icon(&self) -> Option<PathBuf> {
        let icns = self.icon.as_deref()?;
        let meta = std::fs::metadata(icns).ok()?;
        let mut hash = Sha256::new();
        hash.update(icns.as_os_str().as_encoded_bytes());
        hash.update(meta.len().to_le_bytes());
        if let Ok(modified) = meta.modified()
            && let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            hash.update(since.as_nanos().to_le_bytes());
        }
        let key: String = hash.finalize()[..12]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let cached = Paths::get().icons().join(format!("{key}.png"));
        if cached.is_file() {
            return Some(cached);
        }
        let png = icns_to_png(&std::fs::read(icns).ok()?, ICON_SIZE)?;
        std::fs::create_dir_all(cached.parent()?).ok()?;
        crate::util::write_atomic(&cached, &png).ok()?;
        Some(cached)
    }
}

/// Whether `path` names an application bundle.
pub fn is_bundle(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("app"))
        && path.is_dir()
}

/// Read a bundle's `Contents/Info.plist`. A bundle without one still gets a
/// name, from its folder.
pub fn read(bundle: &Path) -> App {
    let fallback = crate::bottle::program_name(bundle);
    let contents = bundle.join("Contents");
    let info = find(&contents, "Info.plist")
        .and_then(|path| Value::from_file(path).ok())
        .and_then(Value::into_dictionary)
        .unwrap_or_default();
    let mut app = from_info(&info, &fallback, &|ext| mime_for_extension(ext));
    app.path = bundle.to_path_buf();
    app.icon = icon_file(&info).and_then(|name| find(&contents.join("Resources"), &name));
    app
}

/// The applications installed under `dir`: bundles in it, and in the folders
/// inside it (such as `Utilities`), sorted by name.
pub fn scan(dir: &Path) -> Vec<App> {
    let mut apps = Vec::new();
    collect(dir, 2, &mut apps);
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

fn collect(dir: &Path, depth: u32, apps: &mut Vec<App>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') || !path.is_dir() {
            continue;
        }
        if is_bundle(&path) {
            apps.push(App {
                installed: true,
                ..read(&path)
            });
        } else if depth > 1 {
            collect(&path, depth - 1, apps);
        }
    }
}

fn string(info: &Dictionary, key: &str) -> Option<String> {
    info.get(key)
        .and_then(Value::as_string)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A key that may hold one string or an array of them.
fn strings(info: &Dictionary, key: &str) -> Vec<String> {
    match info.get(key) {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_string)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn dictionaries<'a>(info: &'a Dictionary, key: &str) -> impl Iterator<Item = &'a Dictionary> {
    info.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_dictionary)
}

/// The name of the bundle's icon file in `Contents/Resources`.
fn icon_file(info: &Dictionary) -> Option<String> {
    let name = string(info, "CFBundleIconFile").or_else(|| string(info, "CFBundleIconName"))?;
    Some(if Path::new(&name).extension().is_some() {
        name
    } else {
        format!("{name}.icns")
    })
}

/// `dir/name`, matched without regard to case the way a Mac would.
fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.exists() {
        return Some(exact);
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
}

/// What an `Info.plist` says. `extension_type` names the MIME type of a file
/// extension, or None.
fn from_info(
    info: &Dictionary,
    fallback_name: &str,
    extension_type: &dyn Fn(&str) -> Option<String>,
) -> App {
    let name = string(info, "CFBundleDisplayName")
        .or_else(|| string(info, "CFBundleName"))
        .unwrap_or_else(|| fallback_name.to_string());

    let mut schemes = BTreeSet::new();
    for url_type in dictionaries(info, "CFBundleURLTypes") {
        for scheme in strings(url_type, "CFBundleURLSchemes") {
            let scheme = scheme.trim().to_ascii_lowercase();
            // Files reach the app through its document types instead.
            if valid_scheme(&scheme) && scheme != "file" {
                schemes.insert(scheme);
            }
        }
    }

    // The types the app declares itself, which its document types may name.
    let mut declared: HashMap<String, Vec<String>> = HashMap::new();
    for key in ["UTExportedTypeDeclarations", "UTImportedTypeDeclarations"] {
        for declaration in dictionaries(info, key) {
            let Some(id) = string(declaration, "UTTypeIdentifier") else {
                continue;
            };
            let Some(tags) = declaration
                .get("UTTypeTagSpecification")
                .and_then(Value::as_dictionary)
            else {
                continue;
            };
            let mut types = strings(tags, "public.mime-type");
            types.extend(
                strings(tags, "public.filename-extension")
                    .iter()
                    .filter_map(|ext| extension_type(ext)),
            );
            declared.entry(id.to_lowercase()).or_default().extend(types);
        }
    }

    let mut mime_types = BTreeSet::new();
    for document in dictionaries(info, "CFBundleDocumentTypes") {
        // `None` declares a type the app knows of but does not open.
        if string(document, "CFBundleTypeRole").as_deref() == Some("None") {
            continue;
        }
        mime_types.extend(strings(document, "CFBundleTypeMIMETypes"));
        for uti in strings(document, "LSItemContentTypes") {
            let uti = uti.to_lowercase();
            match declared.get(&uti) {
                Some(types) => mime_types.extend(types.iter().cloned()),
                None => mime_types.extend(uti_mime_type(&uti).map(str::to_string)),
            }
        }
        for ext in strings(document, "CFBundleTypeExtensions") {
            if ext != "*" && !ext.is_empty() {
                mime_types.extend(extension_type(&ext));
            }
        }
    }
    mime_types.retain(|t| valid_mime_type(t));

    App {
        name,
        version: string(info, "CFBundleShortVersionString")
            .or_else(|| string(info, "CFBundleVersion")),
        category: string(info, "LSApplicationCategoryType"),
        schemes: schemes.into_iter().collect(),
        mime_types: mime_types.into_iter().collect(),
        ..App::default()
    }
}

/// RFC 3986: a letter, then letters, digits, `+`, `-` and `.`.
pub(crate) fn valid_scheme(scheme: &str) -> bool {
    scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// A concrete `type/subtype`. Wildcards and the catch-all binary type would
/// make the app the handler of everything.
pub(crate) fn valid_mime_type(mime: &str) -> bool {
    let Some((kind, sub)) = mime.split_once('/') else {
        return false;
    };
    let token = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "!#$&-^_.+".contains(c))
    };
    token(kind) && token(sub) && mime != "application/octet-stream"
}

/// The MIME type of files ending in `.ext`, from the shared MIME database.
fn mime_for_extension(ext: &str) -> Option<String> {
    let (guess, uncertain) =
        gtk::gio::content_type_guess(Some(format!("file.{ext}")), None::<&[u8]>);
    if uncertain {
        return None;
    }
    gtk::gio::content_type_get_mime_type(&guess).map(Into::into)
}

/// The MIME types of Apple's own common concrete types. Abstract ones, such as
/// `public.image` or `public.data`, have none.
fn uti_mime_type(uti: &str) -> Option<&'static str> {
    Some(match uti {
        "public.plain-text" | "public.utf8-plain-text" | "public.text" => "text/plain",
        "public.html" => "text/html",
        "public.xml" => "application/xml",
        "public.json" => "application/json",
        "public.yaml" => "application/yaml",
        "public.css" => "text/css",
        "public.comma-separated-values-text" => "text/csv",
        "public.rtf" => "application/rtf",
        "net.daringfireball.markdown" => "text/markdown",
        "com.netscape.javascript-source" => "application/javascript",
        "public.c-source" => "text/x-csrc",
        "public.c-header" => "text/x-chdr",
        "public.c-plus-plus-source" => "text/x-c++src",
        "public.objective-c-source" => "text/x-objcsrc",
        "public.swift-source" => "text/x-swift",
        "public.python-script" => "text/x-python",
        "public.shell-script" => "application/x-shellscript",
        "public.vcard" => "text/vcard",
        "com.apple.ical.ics" => "text/calendar",
        "com.adobe.pdf" => "application/pdf",
        "public.png" => "image/png",
        "public.jpeg" => "image/jpeg",
        "com.compuserve.gif" => "image/gif",
        "public.tiff" => "image/tiff",
        "com.microsoft.bmp" => "image/bmp",
        "com.microsoft.ico" => "image/vnd.microsoft.icon",
        "public.svg-image" => "image/svg+xml",
        "org.webmproject.webp" => "image/webp",
        "public.heic" => "image/heic",
        "com.apple.icns" => "image/x-icns",
        "public.mp3" => "audio/mpeg",
        "public.mpeg-4-audio" => "audio/mp4",
        "com.microsoft.waveform-audio" => "audio/x-wav",
        "public.aiff-audio" => "audio/x-aiff",
        "org.xiph.flac" => "audio/flac",
        "public.mpeg-4" => "video/mp4",
        "com.apple.quicktime-movie" => "video/quicktime",
        "public.avi" => "video/x-msvideo",
        "org.webmproject.webm" => "video/webm",
        "public.zip-archive" | "com.pkware.zip-archive" => "application/zip",
        "org.gnu.gnu-zip-archive" => "application/gzip",
        "public.tar-archive" => "application/x-tar",
        "com.apple.disk-image-udif" => "application/x-apple-diskimage",
        "public.truetype-ttf-font" => "font/ttf",
        "public.opentype-font" => "font/otf",
        "com.microsoft.word.doc" => "application/msword",
        "com.microsoft.excel.xls" => "application/vnd.ms-excel",
        "com.microsoft.powerpoint.ppt" => "application/vnd.ms-powerpoint",
        "org.openxmlformats.wordprocessingml.document" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        }
        "org.openxmlformats.spreadsheetml.sheet" => {
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        }
        "org.openxmlformats.presentationml.presentation" => {
            "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        }
        _ => return None,
    })
}

/// One image in an `.icns` file.
struct Image<'a> {
    size: u32,
    kind: Encoding<'a>,
}

enum Encoding<'a> {
    Png(&'a [u8]),
    /// The packed RGB of the old formats, and their 8-bit mask if present.
    Rle(&'a [u8], Option<&'a [u8]>),
}

/// The best image in an `.icns` file as a PNG no bigger than `max` pixels:
/// the largest that fits, or the smallest bigger one scaled down.
///
/// Modern icons hold PNGs. Older ones hold run-length encoded RGB with a
/// separate mask, which is decoded here. JPEG 2000 images are skipped.
fn icns_to_png(data: &[u8], max: u32) -> Option<Vec<u8>> {
    let images = icns_images(data);
    let best = images
        .iter()
        .filter(|i| i.size <= max)
        .max_by_key(|i| (i.size, matches!(i.kind, Encoding::Png(_))))
        .or_else(|| images.iter().min_by_key(|i| i.size))?;
    match best.kind {
        Encoding::Png(png) if best.size <= max => Some(png.to_vec()),
        Encoding::Png(png) => scale_png(png, max),
        Encoding::Rle(rgb, mask) => {
            let rgba = decode_rle(rgb, mask, best.size)?;
            encode_png(rgba, best.size)
        }
    }
}

fn icns_images(data: &[u8]) -> Vec<Image<'_>> {
    let mut images = Vec::new();
    if data.len() < 8 || &data[..4] != b"icns" {
        return images;
    }
    let mut elements = HashMap::new();
    let mut at = 8;
    while at + 8 <= data.len() {
        let kind = &data[at..at + 4];
        let len = u32::from_be_bytes(data[at + 4..at + 8].try_into().unwrap()) as usize;
        if len < 8 || at + len > data.len() {
            break;
        }
        elements.insert(kind, &data[at + 8..at + len]);
        at += len;
    }
    for (kind, body) in &elements {
        if body.starts_with(b"\x89PNG\r\n\x1a\n") && body.len() >= 24 {
            let width = u32::from_be_bytes(body[16..20].try_into().unwrap());
            images.push(Image {
                size: width,
                kind: Encoding::Png(body),
            });
            continue;
        }
        let (size, mask) = match *kind {
            b"is32" => (16, b"s8mk"),
            b"il32" => (32, b"l8mk"),
            b"ih32" => (48, b"h8mk"),
            b"it32" => (128, b"t8mk"),
            _ => continue,
        };
        // `it32` starts with four zero bytes.
        let body = if *kind == b"it32" && body.len() >= 4 {
            &body[4..]
        } else {
            body
        };
        let mask = elements.get(&mask[..]).copied();
        images.push(Image {
            size,
            kind: Encoding::Rle(body, mask),
        });
    }
    images
}

/// The old RGB format: each channel in turn, packed with a simple run-length
/// code. A size's data may also be stored unpacked.
fn decode_rle(data: &[u8], mask: Option<&[u8]>, size: u32) -> Option<Vec<u8>> {
    let pixels = (size * size) as usize;
    let mut rgba = vec![255u8; pixels * 4];
    if data.len() == pixels * 4 {
        // Unpacked ARGB.
        for (i, px) in data.chunks_exact(4).enumerate() {
            rgba[i * 4..i * 4 + 3].copy_from_slice(&px[1..4]);
        }
    } else {
        let mut at = 0;
        for channel in 0..3 {
            let mut filled = 0;
            while filled < pixels {
                let control = *data.get(at)?;
                at += 1;
                if control & 0x80 != 0 {
                    let count = control as usize - 125;
                    let value = *data.get(at)?;
                    at += 1;
                    for _ in 0..count.min(pixels - filled) {
                        rgba[filled * 4 + channel] = value;
                        filled += 1;
                    }
                } else {
                    let count = control as usize + 1;
                    let run = data.get(at..at + count)?;
                    at += count;
                    for value in run.iter().take(pixels - filled) {
                        rgba[filled * 4 + channel] = *value;
                        filled += 1;
                    }
                }
            }
        }
    }
    if let Some(mask) = mask.filter(|m| m.len() >= pixels) {
        for (i, alpha) in mask[..pixels].iter().enumerate() {
            rgba[i * 4 + 3] = *alpha;
        }
    }
    Some(rgba)
}

fn encode_png(rgba: Vec<u8>, size: u32) -> Option<Vec<u8>> {
    let texture = gtk::gdk::MemoryTexture::new(
        size as i32,
        size as i32,
        gtk::gdk::MemoryFormat::R8g8b8a8,
        &gtk::glib::Bytes::from_owned(rgba),
        size as usize * 4,
    );
    Some(texture.save_to_png_bytes().to_vec())
}

fn scale_png(png: &[u8], max: u32) -> Option<Vec<u8>> {
    use gtk::gdk_pixbuf::{InterpType, Pixbuf};
    let stream = gtk::gio::MemoryInputStream::from_bytes(&gtk::glib::Bytes::from(png));
    let pixbuf = Pixbuf::from_stream(&stream, gtk::gio::Cancellable::NONE).ok()?;
    let scaled = pixbuf.scale_simple(max as i32, max as i32, InterpType::Hyper)?;
    scaled.save_to_bufferv("png", &[]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Viewer</string>
  <key>CFBundleDisplayName</key><string>Fancy Viewer</string>
  <key>CFBundleIdentifier</key><string>com.example.viewer</string>
  <key>CFBundleShortVersionString</key><string>2.1</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.puzzle-games</string>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLSchemes</key>
      <array><string>Viewer</string><string>file</string><string>9bad</string></array>
    </dict>
  </array>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSItemContentTypes</key>
      <array><string>public.png</string><string>com.example.scene</string><string>public.image</string></array>
      <key>CFBundleTypeExtensions</key><array><string>vw</string><string>*</string></array>
    </dict>
    <dict>
      <key>CFBundleTypeRole</key><string>None</string>
      <key>CFBundleTypeMIMETypes</key><array><string>text/ignored</string></array>
    </dict>
    <dict>
      <key>CFBundleTypeRole</key><string>Editor</string>
      <key>CFBundleTypeMIMETypes</key><string>application/x-viewer</string>
    </dict>
  </array>
  <key>UTExportedTypeDeclarations</key>
  <array>
    <dict>
      <key>UTTypeIdentifier</key><string>com.example.scene</string>
      <key>UTTypeTagSpecification</key>
      <dict>
        <key>public.mime-type</key><string>application/x-scene</string>
        <key>public.filename-extension</key><array><string>scn</string></array>
      </dict>
    </dict>
  </array>
</dict>
</plist>"#;

    fn extension(ext: &str) -> Option<String> {
        Some(format!("application/x-ext-{ext}"))
    }

    #[test]
    fn reads_info_plist() {
        let info = Value::from_reader_xml(INFO.as_bytes())
            .unwrap()
            .into_dictionary()
            .unwrap();
        let app = from_info(&info, "Folder", &extension);
        assert_eq!(app.name, "Fancy Viewer");
        assert_eq!(app.version.as_deref(), Some("2.1"));
        assert_eq!(app.categories(), "Game;");
        assert_eq!(icon_file(&info).as_deref(), Some("AppIcon.icns"));
        assert_eq!(app.schemes, ["viewer"]);
        assert_eq!(
            app.mime_types,
            [
                "application/x-ext-scn",
                "application/x-ext-vw",
                "application/x-scene",
                "application/x-viewer",
                "image/png",
            ]
        );
        assert_eq!(
            app.handled_types().last().map(String::as_str),
            Some("x-scheme-handler/viewer")
        );
    }

    #[test]
    fn empty_info_falls_back_to_folder_name() {
        let app = from_info(&Dictionary::new(), "Folder", &extension);
        assert_eq!(app.name, "Folder");
        assert!(app.handled_types().is_empty());
        assert_eq!(app.categories(), "");
    }

    #[test]
    fn mime_types_are_checked() {
        assert!(valid_mime_type("image/png"));
        assert!(valid_mime_type("application/vnd.ms-excel"));
        assert!(!valid_mime_type("image/*"));
        assert!(!valid_mime_type("application/octet-stream"));
        assert!(!valid_mime_type("text"));
        assert!(!valid_mime_type("text/plain;charset=utf-8"));
    }

    #[test]
    fn decodes_packed_rgb() {
        // 16x16: each channel is one run of 128 and one of 128 (0x80 + 3 = 131,
        // capped at 128), using both the run and the literal forms.
        let size = 16u32;
        let mut data = Vec::new();
        for value in [10u8, 20, 30] {
            data.extend([0xFD, value, 0xFD, value]);
        }
        let mask = vec![7u8; 256];
        let rgba = decode_rle(&data, Some(&mask), size).unwrap();
        assert_eq!(&rgba[..4], &[10, 20, 30, 7]);
        assert_eq!(&rgba[rgba.len() - 4..], &[10, 20, 30, 7]);

        let literal = [2u8, 1, 2, 3];
        let rgba = decode_rle(&[&literal[..], &[0xFF, 0][..]].concat(), None, 2);
        // Only the red channel is present: the green one runs out.
        assert!(rgba.is_none());
        assert!(decode_rle(&[0x80, 5, 0x80, 6, 0x80, 7], None, 1).is_some());
    }

    #[test]
    fn picks_the_largest_png_that_fits() {
        let png = |size: u32| {
            let mut body = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
            body.extend(size.to_be_bytes());
            body.extend(size.to_be_bytes());
            body
        };
        let element = |kind: &[u8], body: &[u8]| {
            let mut e = kind.to_vec();
            e.extend(((body.len() + 8) as u32).to_be_bytes());
            e.extend(body);
            e
        };
        let elements = [
            element(b"ic08", &png(256)),
            element(b"ic09", &png(512)),
            element(b"JUNK", b"xx"),
        ]
        .concat();
        let mut file = b"icns".to_vec();
        file.extend(((elements.len() + 8) as u32).to_be_bytes());
        file.extend(elements);
        assert_eq!(icns_to_png(&file, 512), Some(png(512)));
        assert_eq!(icns_to_png(&file, 300), Some(png(256)));
        assert_eq!(icns_to_png(b"nope", 512), None);
    }
}
