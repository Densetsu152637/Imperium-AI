//! Files attached to user messages: staging them (size limit, what they are, where they may
//! live) and rendering them for the model.
//!
//! A staged file is referenced where it is when the session's tools may read it there (inside
//! its folders or path grants); anything else is copied into the session's attachments folder,
//! which the session is granted read access to. Rendering reads the files again at the start
//! of the turn: text is inlined up to [`INLINE_TEXT_BYTES`] with a truncation marker, a PDF's
//! text is extracted (and its embedded JPEG page images kept for a vision model), an image goes
//! as an image part for a vision model, and anything else is a path the agent reads with its
//! tools.

use std::io::Read;
use std::path::{Path, PathBuf};

use base64::Engine;
use imperium_protocol::session::{Attachment, AttachmentKind};

/// Text inlined from one attachment before it is cut.
pub const INLINE_TEXT_BYTES: usize = 100 * 1024;
/// Characters of a PDF's extracted text inlined before it is cut.
pub const PDF_TEXT_CHARS: usize = 100_000;
/// Embedded page images taken from one PDF for a vision model.
pub const PDF_IMAGES: usize = 8;
/// Attachments one message may carry.
pub const MAX_ATTACHMENTS: usize = 20;
/// The largest thumbnail a client may journal with an attachment.
pub const PREVIEW_BYTES: usize = 64 * 1024;
/// The largest image returned to a client to draw its thumbnail from.
pub const IMAGE_DATA_BYTES: u64 = 4 * 1024 * 1024;
/// Bytes read from the start of a file to tell what it is.
const SNIFF_BYTES: usize = 8 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum AttachmentError {
    #[error(
        "{name} is {size} bytes; attachments may be at most {limit} bytes (sessions.attachment_max_mib)"
    )]
    TooLarge { name: String, size: u64, limit: u64 },
    #[error("{0} is not a file")]
    NotAFile(String),
    #[error("{0} is empty")]
    Empty(String),
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
    #[error("the attachment's data is not valid base64")]
    BadData,
    #[error("give the attachment's path or its data")]
    NoSource,
    #[error(
        "{0} is no longer in the session's attachments folder or inside its folders; attach it again"
    )]
    Outside(String),
    #[error("a message may carry at most {MAX_ATTACHMENTS} attachments")]
    TooMany,
    #[error("an attachment preview must be a data:image URL of at most {PREVIEW_BYTES} bytes")]
    BadPreview,
}

/// Where a staged file comes from.
#[derive(Debug)]
pub enum Source {
    /// A file on this machine.
    Path(PathBuf),
    /// The file's bytes, base64.
    Base64(String),
}

/// What [`stage`] needs besides the file.
pub struct Staging<'a> {
    /// The session's attachments folder; created when a file is copied into it.
    pub store: &'a Path,
    /// Whether the session's tools may read a (canonical) path where it is.
    pub readable: &'a dyn Fn(&Path) -> bool,
    /// The largest file accepted, in bytes.
    pub max_bytes: u64,
}

/// Stages one file: checks its size, tells what it is, and keeps it where it is when the
/// session may read it there, else copies it into the attachments folder as `<id>-<name>`.
pub fn stage(
    staging: &Staging<'_>,
    id: &str,
    name: &str,
    source: Source,
) -> Result<Attachment, AttachmentError> {
    match source {
        Source::Path(path) => {
            let shown = path.display().to_string();
            let canonical = path
                .canonicalize()
                .map_err(|error| AttachmentError::Io(shown.clone(), error))?;
            let metadata = std::fs::metadata(&canonical)
                .map_err(|error| AttachmentError::Io(shown.clone(), error))?;
            if !metadata.is_file() {
                return Err(AttachmentError::NotAFile(shown));
            }
            let name = if name.trim().is_empty() {
                canonical
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            } else {
                name.to_string()
            };
            check_size(&name, metadata.len(), staging.max_bytes)?;
            let head = read_head(&canonical)?;
            let (kind, mime) = classify(&name, &head);
            let (path, copied) = if (staging.readable)(&canonical) {
                (canonical, false)
            } else {
                let target = store_path(staging.store, id, &name)?;
                std::fs::copy(&canonical, &target)
                    .map_err(|error| AttachmentError::Io(shown, error))?;
                (target, true)
            };
            Ok(attachment(
                id,
                &name,
                kind,
                mime,
                metadata.len(),
                &path,
                copied,
            ))
        }
        Source::Base64(data) => {
            // Base64 is 4/3 of the bytes: refuse an oversized upload before decoding it.
            let decoded_size = (data.len() as u64 / 4) * 3;
            check_size(name, decoded_size.saturating_sub(2), staging.max_bytes)?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data.trim())
                .map_err(|_| AttachmentError::BadData)?;
            let name = if name.trim().is_empty() {
                "attachment"
            } else {
                name
            };
            check_size(name, bytes.len() as u64, staging.max_bytes)?;
            if bytes.is_empty() {
                return Err(AttachmentError::Empty(name.to_string()));
            }
            let (kind, mime) = classify(name, &bytes[..bytes.len().min(SNIFF_BYTES)]);
            let target = store_path(staging.store, id, name)?;
            std::fs::write(&target, &bytes)
                .map_err(|error| AttachmentError::Io(target.display().to_string(), error))?;
            Ok(attachment(
                id,
                name,
                kind,
                mime,
                bytes.len() as u64,
                &target,
                true,
            ))
        }
    }
}

/// Checks an attachment a client sends back with a message: the file must still be in the
/// attachments folder or readable where it is, within the size limit; its kind, type, and size
/// are read again from the file (the client's are not trusted), and its preview must be a
/// small image `data:` URL. `vision` is set from the session's model.
pub fn verify(
    staging: &Staging<'_>,
    sent: &Attachment,
    vision: bool,
) -> Result<Attachment, AttachmentError> {
    let canonical = Path::new(&sent.path)
        .canonicalize()
        .map_err(|_| AttachmentError::Outside(sent.name.clone()))?;
    let store = staging.store.canonicalize().ok();
    let copied = store
        .as_deref()
        .is_some_and(|store| canonical.starts_with(store));
    if !copied && !(staging.readable)(&canonical) {
        return Err(AttachmentError::Outside(sent.name.clone()));
    }
    let metadata = std::fs::metadata(&canonical)
        .map_err(|error| AttachmentError::Io(sent.name.clone(), error))?;
    if !metadata.is_file() {
        return Err(AttachmentError::NotAFile(sent.name.clone()));
    }
    check_size(&sent.name, metadata.len(), staging.max_bytes)?;
    if let Some(preview) = &sent.preview
        && !valid_preview(preview)
    {
        return Err(AttachmentError::BadPreview);
    }
    let head = read_head(&canonical)?;
    let (kind, mime) = classify(&sent.name, &head);
    let mut checked = attachment(
        &sent.id,
        &sent.name,
        kind,
        mime,
        metadata.len(),
        &canonical,
        copied,
    );
    checked.vision = vision;
    checked.preview = sent.preview.clone();
    Ok(checked)
}

/// Checks a message's attachments with [`verify`].
pub fn verify_all(
    staging: &Staging<'_>,
    sent: &[Attachment],
    vision: bool,
) -> Result<Vec<Attachment>, AttachmentError> {
    if sent.len() > MAX_ATTACHMENTS {
        return Err(AttachmentError::TooMany);
    }
    sent.iter()
        .map(|attachment| verify(staging, attachment, vision))
        .collect()
}

/// An image file's bytes as a `data:` URL when it is small enough for a client thumbnail.
pub fn image_data(attachment: &Attachment) -> Option<String> {
    if attachment.kind != AttachmentKind::Image || attachment.size > IMAGE_DATA_BYTES {
        return None;
    }
    let bytes = std::fs::read(&attachment.path).ok()?;
    Some(data_url(&attachment.mime, &bytes))
}

fn valid_preview(preview: &str) -> bool {
    preview.len() <= PREVIEW_BYTES
        && ["png", "jpeg", "webp", "gif"]
            .iter()
            .any(|kind| preview.starts_with(&format!("data:image/{kind};base64,")))
}

fn check_size(name: &str, size: u64, limit: u64) -> Result<(), AttachmentError> {
    if size > limit {
        return Err(AttachmentError::TooLarge {
            name: name.to_string(),
            size,
            limit,
        });
    }
    Ok(())
}

fn read_head(path: &Path) -> Result<Vec<u8>, AttachmentError> {
    let mut head = Vec::with_capacity(SNIFF_BYTES);
    std::fs::File::open(path)
        .and_then(|file| file.take(SNIFF_BYTES as u64).read_to_end(&mut head))
        .map_err(|error| AttachmentError::Io(path.display().to_string(), error))?;
    Ok(head)
}

/// `<store>/<id>-<safe name>`, with the folder created.
fn store_path(store: &Path, id: &str, name: &str) -> Result<PathBuf, AttachmentError> {
    std::fs::create_dir_all(store)
        .map_err(|error| AttachmentError::Io(store.display().to_string(), error))?;
    Ok(store.join(format!("{id}-{}", safe_name(name))))
}

/// The last component of `name` with anything but letters, digits, `.`, `-`, `_`, and spaces
/// replaced, at most 100 characters; `attachment` when nothing is left.
pub fn safe_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = last
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .take(100)
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if trimmed.is_empty() {
        "attachment".into()
    } else {
        trimmed.to_string()
    }
}

fn attachment(
    id: &str,
    name: &str,
    kind: AttachmentKind,
    mime: String,
    size: u64,
    path: &Path,
    copied: bool,
) -> Attachment {
    Attachment {
        id: id.to_string(),
        name: name.to_string(),
        kind,
        mime,
        size,
        path: plain(path),
        copied,
        vision: false,
        preview: None,
    }
}

/// A path as people write it, without Windows' `\\?\` verbatim prefix.
fn plain(path: &Path) -> String {
    let shown = path.display().to_string();
    match shown.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => rest.to_string(),
        _ => shown,
    }
}

/// What a file is, from its first bytes (and its name for text types): images and PDFs by
/// their signatures, UTF-8 without NUL bytes as text, anything else a file.
pub fn classify(name: &str, head: &[u8]) -> (AttachmentKind, String) {
    let signatures: [(&[u8], &str); 4] = [
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"\xff\xd8\xff", "image/jpeg"),
        (b"GIF87a", "image/gif"),
        (b"GIF89a", "image/gif"),
    ];
    if let Some((_, mime)) = signatures
        .iter()
        .find(|(signature, _)| head.starts_with(signature))
    {
        return (AttachmentKind::Image, (*mime).into());
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return (AttachmentKind::Image, "image/webp".into());
    }
    if head.starts_with(b"%PDF-") {
        return (AttachmentKind::Pdf, "application/pdf".into());
    }
    let extension = Path::new(name)
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if looks_like_text(head) {
        let mime = match extension.as_str() {
            "md" | "markdown" => "text/markdown",
            "json" => "application/json",
            "csv" => "text/csv",
            "html" | "htm" => "text/html",
            "xml" => "application/xml",
            "toml" => "application/toml",
            "yaml" | "yml" => "application/yaml",
            _ => "text/plain",
        };
        return (AttachmentKind::Text, mime.into());
    }
    let mime = match extension.as_str() {
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    };
    (AttachmentKind::File, mime.into())
}

/// UTF-8 (allowing a character cut off at the end of the sample) without NUL bytes.
fn looks_like_text(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(error) => error.error_len().is_none() && head.len() - error.valid_up_to() < 4,
    }
}

fn data_url(mime: &str, bytes: &[u8]) -> String {
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// A message's attachments as the model reads them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rendered {
    /// An `<attachments>` block for the message's text; empty without attachments.
    pub text: String,
    /// Images for a vision model, as `data:` URLs.
    pub images: Vec<String>,
}

/// Reads the attachments for the model (blocking: run it off the async runtime). A file that
/// cannot be read any more is reported in the block rather than failing the turn.
pub fn render(attachments: &[Attachment]) -> Rendered {
    let mut rendered = Rendered::default();
    if attachments.is_empty() {
        return rendered;
    }
    let mut text = String::from("<attachments>\n");
    for attachment in attachments {
        let body = match render_one(attachment, &mut rendered.images) {
            Ok(body) => body,
            Err(error) => format!("[The file could not be read: {error}]"),
        };
        text.push_str(&format!(
            "<attachment name=\"{}\" type=\"{}\" path=\"{}\">\n{}\n</attachment>\n",
            attribute(&attachment.name),
            attachment.mime,
            attribute(&attachment.path),
            body.trim_end()
        ));
    }
    text.push_str("</attachments>");
    rendered.text = text;
    rendered
}

fn attribute(value: &str) -> String {
    value.replace('"', "&quot;")
}

/// Opens the file an attachment names, which [`verify`] checked when the message was sent.
/// Messages queue, so the file can have changed since: the path must still be the canonical
/// one that was checked (a link or junction put in its place, or in a folder above it, makes
/// it resolve elsewhere) and must still be a regular file, so a queued attachment cannot be
/// swapped for a file outside the session's folders or for a device that never ends.
fn open_verified(attachment: &Attachment) -> std::io::Result<std::fs::File> {
    let canonical = Path::new(&attachment.path).canonicalize()?;
    if plain(&canonical) != attachment.path {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "it is no longer where it was attached (it was replaced by a link or moved); attach it again",
        ));
    }
    let file = std::fs::File::open(&canonical)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "it is no longer a regular file",
        ));
    }
    Ok(file)
}

/// The whole of an image or PDF attachment, at most the size [`verify`] saw: a file that has
/// grown since is refused rather than read without bound.
fn read_verified(attachment: &Attachment) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_verified(attachment)?
        .take(attachment.size.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > attachment.size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "it has grown since it was attached; attach it again",
        ));
    }
    Ok(bytes)
}

fn render_one(attachment: &Attachment, images: &mut Vec<String>) -> std::io::Result<String> {
    Ok(match attachment.kind {
        AttachmentKind::Text => {
            let mut bytes = Vec::new();
            let file = open_verified(attachment)?;
            let size = file.metadata().map_or(attachment.size, |m| m.len());
            file.take(INLINE_TEXT_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            inline_text(&bytes, size, &attachment.path)
        }
        AttachmentKind::Image if attachment.vision => {
            images.push(data_url(&attachment.mime, &read_verified(attachment)?));
            "[Image attached: it is shown to you with this message.]".into()
        }
        AttachmentKind::Image => "[Image attached. Your model cannot view images, so it is not \
             shown; the file is at the path above for tools that can read it.]"
            .into(),
        AttachmentKind::Pdf => {
            let bytes = read_verified(attachment)?;
            let (text, pages) = pdf_text(&bytes);
            let mut body = match text {
                Some(text) if !text.trim().is_empty() => {
                    cut_chars(text.trim(), PDF_TEXT_CHARS, &attachment.path)
                }
                Some(_) => "[No text layer was found in this PDF (it may be scanned).]".into(),
                None => "[The PDF's text could not be extracted.]".into(),
            };
            if attachment.vision {
                let found = pdf_images(&bytes, PDF_IMAGES);
                if !found.is_empty() {
                    body.push_str(&format!(
                        "\n[{} page image{} from this PDF {} shown to you with this message.]",
                        found.len(),
                        if found.len() == 1 { "" } else { "s" },
                        if found.len() == 1 { "is" } else { "are" },
                    ));
                    images.extend(found);
                }
            }
            if let Some(pages) = pages {
                body = format!(
                    "[{pages} page{}]\n{body}",
                    if pages == 1 { "" } else { "s" }
                );
            }
            body
        }
        AttachmentKind::File => format!(
            "[A {} file of {} bytes. It is not inlined; read it with your tools if you need it.]",
            attachment.mime, attachment.size
        ),
    })
}

/// Up to [`INLINE_TEXT_BYTES`] of `bytes` (cut at a character boundary), with a marker naming
/// how much was left out when the file (`size` bytes) is longer.
pub fn inline_text(bytes: &[u8], size: u64, path: &str) -> String {
    if bytes.len() <= INLINE_TEXT_BYTES && size as usize <= INLINE_TEXT_BYTES {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut end = INLINE_TEXT_BYTES.min(bytes.len());
    while end > 0 && std::str::from_utf8(&bytes[..end]).is_err() {
        end -= 1;
    }
    let shown = String::from_utf8_lossy(&bytes[..end]);
    format!(
        "{shown}\n[… truncated: showing the first {end} of {size} bytes. The whole file is at \
         {path}; read the rest with your file tools.]"
    )
}

fn cut_chars(text: &str, limit: usize, path: &str) -> String {
    match text.char_indices().nth(limit) {
        None => text.to_string(),
        Some((end, _)) => format!(
            "{}\n[… truncated: showing the first {limit} characters of the extracted text. The \
             PDF is at {path}.]",
            &text[..end]
        ),
    }
}

/// A PDF's text and page count; `None` text when extraction failed. The extractor panics on
/// some malformed files, so it runs behind `catch_unwind`.
pub fn pdf_text(bytes: &[u8]) -> (Option<String>, Option<usize>) {
    let pages = pdf_extract::Document::load_mem(bytes)
        .ok()
        .map(|document| document.get_pages().len());
    let text = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(bytes))
        .ok()
        .and_then(Result::ok);
    (text, pages)
}

/// Up to `limit` JPEG images embedded in the PDF's pages (a scanned page is usually one), as
/// `data:` URLs. Images in other encodings would need decoding and are skipped.
pub fn pdf_images(bytes: &[u8], limit: usize) -> Vec<String> {
    let Ok(document) = pdf_extract::Document::load_mem(bytes) else {
        return Vec::new();
    };
    let mut images = Vec::new();
    for page in document.get_pages().into_values() {
        let Ok(found) = document.get_page_images(page) else {
            continue;
        };
        for image in found {
            let jpeg = image
                .filters
                .as_ref()
                .is_some_and(|filters| filters.len() == 1 && filters[0] == "DCTDecode");
            if jpeg {
                images.push(data_url("image/jpeg", image.content));
                if images.len() == limit {
                    return images;
                }
            }
        }
    }
    images
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";

    /// A one-page PDF whose page shows "Hello attachments" in Helvetica, with a correct
    /// cross-reference table.
    fn tiny_pdf() -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 144] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            {
                let stream = "BT /F1 18 Tf 20 60 Td (Hello attachments) Tj ET";
                format!(
                    "<< /Length {} >>\nstream\n{stream}\nendstream",
                    stream.len()
                )
            },
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_string(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    fn staging<'a>(store: &'a Path, readable: &'a dyn Fn(&Path) -> bool) -> Staging<'a> {
        Staging {
            store,
            readable,
            max_bytes: 1024,
        }
    }

    #[test]
    fn files_are_told_apart_by_their_contents() {
        assert_eq!(classify("x.bin", PNG).0, AttachmentKind::Image);
        assert_eq!(
            classify("photo", b"\xff\xd8\xff\xe0...").1,
            "image/jpeg".to_string()
        );
        assert_eq!(
            classify("a", b"RIFF\0\0\0\0WEBPVP8 ").1,
            "image/webp".to_string()
        );
        assert_eq!(classify("doc", b"%PDF-1.7\n").0, AttachmentKind::Pdf);
        assert_eq!(
            classify("notes.md", "# Titre — été".as_bytes()),
            (AttachmentKind::Text, "text/markdown".into())
        );
        // A multi-byte character cut off at the end of the sample is still text.
        assert_eq!(
            classify("a.txt", &"é".as_bytes()[..1]).0,
            AttachmentKind::Text
        );
        assert_eq!(classify("a.png", b"not\0text").0, AttachmentKind::File);
        // A name alone does not make an image.
        assert_eq!(classify("fake.png", b"hello").0, AttachmentKind::Text);
    }

    #[test]
    fn files_inside_the_session_stay_where_they_are_and_others_are_copied() {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let store = store.path().join("s1");
        let inside_root = project.path().canonicalize().unwrap();
        let readable = move |path: &Path| path.starts_with(&inside_root);
        let staging = staging(&store, &readable);

        let local = project.path().join("main.rs");
        std::fs::write(&local, "fn main() {}").unwrap();
        let kept = stage(&staging, "a1", "", Source::Path(local.clone())).unwrap();
        assert!(!kept.copied);
        assert_eq!(kept.name, "main.rs");
        assert_eq!(kept.kind, AttachmentKind::Text);
        assert_eq!(
            Path::new(&kept.path).canonicalize().unwrap(),
            local.canonicalize().unwrap()
        );

        let far = outside.path().join("shot.png");
        std::fs::write(&far, PNG).unwrap();
        let copied = stage(&staging, "a2", "shot.png", Source::Path(far)).unwrap();
        assert!(copied.copied);
        assert_eq!(copied.kind, AttachmentKind::Image);
        assert!(
            Path::new(&copied.path).starts_with(&store),
            "{}",
            copied.path
        );
        assert_eq!(std::fs::read(&copied.path).unwrap(), PNG);

        let pasted = stage(
            &staging,
            "a3",
            "../../evil name?.txt",
            Source::Base64(base64::engine::general_purpose::STANDARD.encode("hello")),
        )
        .unwrap();
        assert!(pasted.copied);
        assert!(
            pasted.path.ends_with("a3-evil name_.txt"),
            "{}",
            pasted.path
        );
        assert_eq!(std::fs::read_to_string(&pasted.path).unwrap(), "hello");
    }

    #[test]
    fn oversized_empty_and_malformed_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        let nothing = |_: &Path| false;
        let staging = staging(&store, &nothing);
        let big = dir.path().join("big.txt");
        std::fs::write(&big, vec![b'a'; 2048]).unwrap();
        assert!(matches!(
            stage(&staging, "a1", "", Source::Path(big)),
            Err(AttachmentError::TooLarge { size: 2048, .. })
        ));
        let encoded = base64::engine::general_purpose::STANDARD.encode(vec![b'a'; 2048]);
        assert!(matches!(
            stage(&staging, "a2", "x", Source::Base64(encoded)),
            Err(AttachmentError::TooLarge { .. })
        ));
        assert!(matches!(
            stage(&staging, "a3", "x", Source::Base64("***".into())),
            Err(AttachmentError::BadData)
        ));
        assert!(matches!(
            stage(&staging, "a4", "x", Source::Base64(String::new())),
            Err(AttachmentError::Empty(_))
        ));
        assert!(matches!(
            stage(&staging, "a5", "", Source::Path(dir.path().to_path_buf())),
            Err(AttachmentError::NotAFile(_))
        ));
    }

    #[test]
    fn sent_attachments_are_checked_again_and_their_claims_replaced() {
        let project = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        let readable = move |path: &Path| path.starts_with(&root);
        let staging = staging(store.path(), &readable);
        let file = project.path().join("a.png");
        std::fs::write(&file, PNG).unwrap();
        let staged = stage(&staging, "a1", "", Source::Path(file)).unwrap();

        // The client claims another kind and size and adds a preview: kind and size are read
        // again, the preview is kept, vision is the daemon's.
        let mut sent = staged.clone();
        sent.kind = AttachmentKind::Text;
        sent.size = 1;
        sent.preview = Some("data:image/jpeg;base64,AA==".into());
        let checked = verify(&staging, &sent, true).unwrap();
        assert_eq!(checked.kind, AttachmentKind::Image);
        assert_eq!(checked.size, PNG.len() as u64);
        assert!(checked.vision);
        assert_eq!(
            checked.preview.as_deref(),
            Some("data:image/jpeg;base64,AA==")
        );

        // A path outside the session's folders and its attachments folder is refused.
        let secret = other.path().join("id_rsa");
        std::fs::write(&secret, "key").unwrap();
        let mut smuggled = staged.clone();
        smuggled.path = secret.display().to_string();
        assert!(matches!(
            verify(&staging, &smuggled, false),
            Err(AttachmentError::Outside(_))
        ));
        // So is a path that climbs out of the attachments folder.
        let mut climbing = staged.clone();
        climbing.path = store
            .path()
            .join("..")
            .join(other.path().file_name().unwrap())
            .join("id_rsa")
            .display()
            .to_string();
        assert!(verify(&staging, &climbing, false).is_err());
        // A preview that is not a small image data URL is refused.
        let mut scripted = staged.clone();
        scripted.preview = Some("javascript:alert(1)".into());
        assert!(matches!(
            verify(&staging, &scripted, false),
            Err(AttachmentError::BadPreview)
        ));
        let many = vec![staged; MAX_ATTACHMENTS + 1];
        assert!(matches!(
            verify_all(&staging, &many, false),
            Err(AttachmentError::TooMany)
        ));
    }

    #[test]
    fn long_text_is_cut_with_a_marker_at_a_character_boundary() {
        let short = inline_text(b"hello", 5, "/p/a.txt");
        assert_eq!(short, "hello");
        let mut long = "é".repeat(INLINE_TEXT_BYTES / 2).into_bytes();
        long.push(b'x');
        long.extend_from_slice(&[b'y'; 10]);
        let cut = inline_text(&long, 200_000, "/p/a.txt");
        assert!(
            cut.contains("[… truncated: showing the first"),
            "{}",
            &cut[cut.len() - 200..]
        );
        assert!(cut.contains("of 200000 bytes"));
        assert!(cut.contains("/p/a.txt"));
        assert!(!cut.contains('\u{fffd}'));
    }

    #[test]
    fn pdf_text_is_extracted_from_a_tiny_fixture() {
        let pdf = tiny_pdf();
        let (text, pages) = pdf_text(&pdf);
        assert_eq!(pages, Some(1));
        assert!(
            text.as_deref()
                .unwrap_or_default()
                .contains("Hello attachments"),
            "{text:?}"
        );
        assert!(pdf_images(&pdf, 4).is_empty());
        // A broken file is reported, not a panic.
        let (broken, _) = pdf_text(b"%PDF-1.4\ngarbage");
        assert!(broken.is_none());
    }

    #[test]
    fn rendering_inlines_text_and_pdfs_and_routes_images_by_vision() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            // As `verify` keeps it: canonical.
            plain(&path.canonicalize().unwrap())
        };
        let item = |name: &str, kind, mime: &str, path: String, vision| Attachment {
            id: name.into(),
            name: name.into(),
            kind,
            mime: mime.into(),
            size: std::fs::metadata(&path).unwrap().len(),
            path,
            copied: false,
            vision,
            preview: None,
        };
        let attachments = vec![
            item(
                "notes.txt",
                AttachmentKind::Text,
                "text/plain",
                write("notes.txt", b"remember the milk"),
                false,
            ),
            item(
                "doc.pdf",
                AttachmentKind::Pdf,
                "application/pdf",
                write("doc.pdf", &tiny_pdf()),
                true,
            ),
            item(
                "seen.png",
                AttachmentKind::Image,
                "image/png",
                write("seen.png", PNG),
                true,
            ),
            item(
                "unseen.png",
                AttachmentKind::Image,
                "image/png",
                write("unseen.png", PNG),
                false,
            ),
            item(
                "tool.bin",
                AttachmentKind::File,
                "application/octet-stream",
                write("tool.bin", b"\0\x01"),
                false,
            ),
        ];
        let rendered = render(&attachments);
        assert!(rendered.text.starts_with("<attachments>\n"));
        assert!(rendered.text.ends_with("</attachments>"));
        assert!(rendered.text.contains("remember the milk"));
        assert!(rendered.text.contains("Hello attachments"));
        assert!(rendered.text.contains("cannot view images"));
        assert!(rendered.text.contains("read it with your tools"));
        assert_eq!(rendered.images.len(), 1);
        assert!(rendered.images[0].starts_with("data:image/png;base64,"));
        assert_eq!(render(&[]), Rendered::default());
    }

    /// Makes `link` a link to the folder `target` (a junction on Windows, which needs no
    /// privilege); `false` where links cannot be made.
    fn link_dir(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(target, link);
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(target, link).or_else(|_| {
            let status = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()?
                .status;
            if status.success() {
                Ok(())
            } else {
                Err(std::io::Error::other("mklink failed"))
            }
        });
        made.is_ok()
    }

    #[test]
    fn a_queued_attachment_swapped_for_a_link_out_of_the_folders_is_not_read() {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        let readable = move |path: &Path| path.starts_with(&root);
        let staging = staging(store.path(), &readable);
        std::fs::create_dir(project.path().join("docs")).unwrap();
        std::fs::write(project.path().join("docs/spec.txt"), "the spec").unwrap();
        std::fs::write(outside.path().join("spec.txt"), "SECRET outside text").unwrap();
        let sent = stage(
            &staging,
            "a1",
            "",
            Source::Path(project.path().join("docs/spec.txt")),
        )
        .unwrap();
        let verified = verify(&staging, &sent, false).unwrap();
        assert!(
            render(std::slice::from_ref(&verified))
                .text
                .contains("the spec")
        );

        // Before the turn starts, `docs` becomes a link to a folder outside the session.
        std::fs::remove_dir_all(project.path().join("docs")).unwrap();
        if !link_dir(outside.path(), &project.path().join("docs")) {
            return;
        }
        let rendered = render(std::slice::from_ref(&verified));
        assert!(
            rendered.text.contains("could not be read"),
            "{}",
            rendered.text
        );
        assert!(!rendered.text.contains("SECRET"), "{}", rendered.text);
    }

    #[test]
    fn an_attachment_that_grew_or_stopped_being_a_file_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.png");
        std::fs::write(&path, PNG).unwrap();
        let canonical = plain(&path.canonicalize().unwrap());
        let png = Attachment {
            id: "a1".into(),
            name: "big.png".into(),
            kind: AttachmentKind::Image,
            mime: "image/png".into(),
            size: PNG.len() as u64,
            path: canonical.clone(),
            copied: false,
            vision: true,
            preview: None,
        };
        assert_eq!(render(std::slice::from_ref(&png)).images.len(), 1);

        std::fs::write(&path, vec![0_u8; PNG.len() + 1000]).unwrap();
        let grown = render(std::slice::from_ref(&png));
        assert!(grown.images.is_empty());
        assert!(grown.text.contains("grown since"), "{}", grown.text);

        // A folder in its place is not a file.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let folder = render(std::slice::from_ref(&png));
        assert!(folder.images.is_empty());
        assert!(folder.text.contains("could not be read"), "{}", folder.text);
    }

    #[test]
    fn names_are_made_safe_for_the_attachments_folder() {
        assert_eq!(
            safe_name("C:\\Users\\me\\report final.pdf"),
            "report final.pdf"
        );
        assert_eq!(safe_name("../.."), "attachment");
        assert_eq!(safe_name("a<b>:c|d.txt"), "a_b__c_d.txt");
    }
}
