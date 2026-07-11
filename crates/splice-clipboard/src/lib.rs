use splice_core::{ImagePaste, PastePayload};
use std::{
    fs,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardPayload {
    Empty,
    Text(String),
    Image(ImagePaste),
}

impl ClipboardPayload {
    pub fn prefers_image(&self) -> bool {
        matches!(self, Self::Image(_))
    }
}

#[derive(Debug)]
pub enum ClipboardError {
    #[cfg(windows)]
    Windows(windows::core::Error),
    Io(std::io::Error),
    EmptyClipboardImage,
    Image(image::ImageError),
    UnsupportedPlatform,
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(windows)]
            Self::Windows(error) => write!(f, "Windows clipboard error: {error}"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::EmptyClipboardImage => write!(f, "clipboard does not contain image data"),
            Self::Image(error) => write!(f, "clipboard image decode/encode error: {error}"),
            Self::UnsupportedPlatform => {
                write!(f, "clipboard image extraction is only supported on Windows")
            }
        }
    }
}

impl std::error::Error for ClipboardError {}

impl From<std::io::Error> for ClipboardError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<image::ImageError> for ClipboardError {
    fn from(error: image::ImageError) -> Self {
        Self::Image(error)
    }
}

#[cfg(windows)]
impl From<windows::core::Error> for ClipboardError {
    fn from(error: windows::core::Error) -> Self {
        Self::Windows(error)
    }
}

pub fn read_clipboard_image_to_temp_dir(
    temp_dir: &Path,
) -> Result<ClipboardPayload, ClipboardError> {
    let dib = read_clipboard_dib_bytes()?;
    Ok(ClipboardPayload::Image(image_paste_from_dib(
        temp_dir, &dib,
    )?))
}

pub fn read_clipboard_image_paste_payload(temp_dir: &Path) -> Result<PastePayload, ClipboardError> {
    match read_clipboard_image_to_temp_dir(temp_dir)? {
        ClipboardPayload::Image(image) => Ok(PastePayload::Image(image)),
        ClipboardPayload::Text(text) => Ok(PastePayload::Text(text)),
        ClipboardPayload::Empty => Err(ClipboardError::EmptyClipboardImage),
    }
}

fn image_paste_from_dib(temp_dir: &Path, dib: &[u8]) -> Result<ImagePaste, ClipboardError> {
    let path = persist_dib_as_png(temp_dir, dib)?;
    Ok(ImagePaste {
        path: path.to_string_lossy().into_owned(),
        mime_type: "image/png".to_owned(),
    })
}

fn persist_dib_as_png(temp_dir: &Path, dib: &[u8]) -> Result<std::path::PathBuf, ClipboardError> {
    // Decode + encode fully before touching the filesystem so a decode error
    // never leaves a partial or stale file behind.
    let png = dib_to_png(dib)?;
    fs::create_dir_all(temp_dir)?;

    let path = temp_dir.join(unique_clipboard_image_name());
    fs::write(&path, png)?;
    Ok(path)
}

// Byte offset WITHIN a raw DIB where the pixel data begins.
//
// BmpDecoder's new_without_file_header entry point GUESSES this offset, and for
// BI_BITFIELDS with a V4/V5 header it unconditionally assumes the Windows
// "long" layout (a redundant 12-byte RGB mask trailer after the header, pixels
// at header+12). That corrupts the equally valid "short" layout (pixels
// immediately after the header) that apps like Paint.NET write. We compute the
// real offset ourselves and hand it to the decoder via a synthetic BMP file
// header (bfOffBits), which the decoder treats as authoritative — so the guess
// never runs.
//
// All reads are length-guarded: a header too short to parse yields a best-effort
// offset and the decode itself fails cleanly downstream (never a panic).
fn dib_pixel_offset(dib: &[u8]) -> usize {
    let read_u32 = |at: usize| -> Option<u32> {
        dib.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let read_i32 = |at: usize| -> Option<i32> {
        dib.get(at..at + 4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let read_u16 = |at: usize| -> Option<u16> {
        dib.get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };

    // A DIB missing its header size field is malformed; return 0 and let the
    // decoder surface the error.
    let Some(header_size) = read_u32(0).map(|value| value as usize) else {
        return 0;
    };
    let width = read_i32(4).unwrap_or(0);
    let height = read_i32(8).unwrap_or(0);
    let bit_count = read_u16(14).unwrap_or(0);
    let compression = read_u32(16).unwrap_or(0);
    let clr_used = read_u32(32).unwrap_or(0);

    // Indexed formats (1/4/8 bpp) carry a color palette between the header and the
    // pixels. clr_used == 0 is the Windows convention for "full 2^bpp palette", so a
    // naive clr_used*4 would compute zero palette bytes and land the pixel offset
    // inside the palette — silently decoding garbage. Default to the full table.
    let palette_entries = if (1..=8).contains(&bit_count) && clr_used == 0 {
        1usize << bit_count
    } else {
        clr_used as usize
    };
    let palette_bytes = palette_entries.saturating_mul(4);
    let candidate = header_size.saturating_add(palette_bytes);

    // BI_BITFIELDS
    if compression == 3 {
        // Classic BITMAPINFOHEADER always carries a real 12-byte mask block after
        // the header; pixels follow it.
        if header_size == 40 {
            return candidate.saturating_add(12);
        }

        // V4 (108) / V5 (124): the masks already live inside the header, so a
        // trailing 12-byte block is redundant and MAY or MAY NOT be present.
        // Disambiguate: treat as "long" only if the DIB is big enough to hold a
        // trailer PLUS the full pixel buffer AND the 12 bytes at the header end
        // actually equal the header's R/G/B masks. Size alone is unreliable
        // (GlobalSize over-reports allocation granularity), hence the mask check.
        if header_size >= 108 {
            let stride = (((width as i64) * (bit_count as i64) + 31) / 32 * 4).max(0) as usize;
            let expected = stride.saturating_mul(height.unsigned_abs() as usize);

            let masks_match = || {
                read_u32(40) == read_u32(candidate)
                    && read_u32(44) == read_u32(candidate + 4)
                    && read_u32(48) == read_u32(candidate + 8)
                    && read_u32(40).is_some()
            };

            let is_long = dib
                .len()
                .checked_sub(candidate)
                .is_some_and(|remaining| remaining >= 12usize.saturating_add(expected))
                && masks_match();

            return if is_long {
                candidate.saturating_add(12)
            } else {
                candidate
            };
        }

        // V2 (52) / V3 (56) BITFIELDS: no trailing skip.
        return candidate;
    }

    // BI_RGB and everything else: pixels start right after header + palette.
    candidate
}

// A DIB whose size or pixel offset exceeds the u32 range of the BMP file header
// cannot be represented; surface a clean error rather than wrapping silently.
fn overflow_error(field: &str) -> ClipboardError {
    ClipboardError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("clipboard DIB too large to encode BMP header field {field}"),
    ))
}

fn dib_to_png(dib: &[u8]) -> Result<Vec<u8>, ClipboardError> {
    // The clipboard hands us a raw DIB (BITMAPINFOHEADER/…/BITMAPV5HEADER + pixel
    // data) with no BMP file header. Rather than let BmpDecoder guess where the
    // pixels start (which mishandles short-layout BITFIELDS DIBV5), we compute the
    // real pixel offset and prepend a 14-byte BITMAPFILEHEADER whose bfOffBits
    // points at it. BmpDecoder::new treats bfOffBits as the authoritative pixel
    // offset, so the layout is handled correctly regardless of header flavor.
    let pixel_offset = dib_pixel_offset(dib);

    // The BMP file header stores bfSize and bfOffBits as u32. A DIB larger than
    // ~4 GiB (or an offset past u32) cannot be represented; fail cleanly instead of
    // silently wrapping via `as u32` and emitting a corrupt header.
    let bf_size = u32::try_from(14 + dib.len()).map_err(|_| overflow_error("bfSize"))?;
    let bf_off_bits = u32::try_from(14 + pixel_offset).map_err(|_| overflow_error("bfOffBits"))?;

    let mut bmp = Vec::with_capacity(14 + dib.len());
    bmp.extend_from_slice(b"BM"); // bfType
    bmp.extend_from_slice(&bf_size.to_le_bytes()); // bfSize
    bmp.extend_from_slice(&0u16.to_le_bytes()); // bfReserved1
    bmp.extend_from_slice(&0u16.to_le_bytes()); // bfReserved2
    bmp.extend_from_slice(&bf_off_bits.to_le_bytes()); // bfOffBits
    bmp.extend_from_slice(dib);

    let decoder = image::codecs::bmp::BmpDecoder::new(std::io::Cursor::new(&bmp))?;
    let decoded = image::DynamicImage::from_decoder(decoder)?;

    // Zero-alpha fixup: some Windows sources (Snipping Tool, GDI screenshots) emit a
    // DIBV5 with an alpha mask set but leave every alpha byte at 0, which decodes as a
    // fully-transparent image. When EVERY pixel is transparent we treat the image as
    // opaque (drop to RGB); otherwise the non-zero alpha is genuine and preserved.
    // Tradeoff: a deliberately fully-transparent image also becomes opaque — acceptable,
    // since such a paste would be invisible and useless anyway.
    let normalized = match decoded {
        image::DynamicImage::ImageRgba8(rgba) if rgba.pixels().all(|pixel| pixel.0[3] == 0) => {
            image::DynamicImage::ImageRgb8(image::DynamicImage::ImageRgba8(rgba).into_rgb8())
        }
        other => other,
    };

    let mut png = Vec::new();
    normalized.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    Ok(png)
}

// Normalize line endings to CRLF, the Windows clipboard convention, and encode
// the result as UTF-16 code units with a trailing NUL — the exact buffer shape
// CF_UNICODETEXT requires. Existing CRLF sequences are collapsed to LF first so
// mixed input never becomes CRCRLF. Kept pure and platform-agnostic so it can be
// unit-tested off Windows; the Win32 SetClipboardData path is Windows-only.
fn encode_clipboard_text_utf16(text: &str) -> Vec<u16> {
    let normalized = text.replace("\r\n", "\n").replace('\n', "\r\n");
    let mut units: Vec<u16> = normalized.encode_utf16().collect();
    units.push(0);
    units
}

// Decode UTF-16 code units (as read from a CF_UNICODETEXT global) into a String,
// stopping at the first NUL terminator. CF_UNICODETEXT buffers are NUL-terminated
// and the backing GlobalAlloc block can be larger than the string, so everything
// from the first NUL onward is allocation slack and must be dropped. Lossy
// decoding maps any unpaired surrogate to U+FFFD so a malformed clipboard payload
// yields text rather than an error. Kept pure and platform-agnostic so it can be
// unit-tested off Windows; the Win32 GetClipboardData path is Windows-only.
fn decode_clipboard_text_utf16(units: &[u16]) -> String {
    let end = units
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

#[cfg(not(windows))]
pub fn write_clipboard_text(_text: &str) -> Result<(), ClipboardError> {
    Err(ClipboardError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn write_clipboard_text(text: &str) -> Result<(), ClipboardError> {
    let utf16 = encode_clipboard_text_utf16(text);
    windows_clipboard::write_unicode_text(&utf16)
}

#[cfg(not(windows))]
pub fn read_clipboard_text() -> Result<String, ClipboardError> {
    Err(ClipboardError::UnsupportedPlatform)
}

// Read CF_UNICODETEXT from the OS clipboard. When no text format is present (e.g.
// the clipboard holds only an image), returns Ok(String::new()) so callers can
// cleanly fall through to the image paste route instead of treating "no text" as
// an error.
#[cfg(windows)]
pub fn read_clipboard_text() -> Result<String, ClipboardError> {
    windows_clipboard::read_unicode_text()
}

#[cfg(not(windows))]
pub fn read_clipboard_dib_bytes() -> Result<Vec<u8>, ClipboardError> {
    Err(ClipboardError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn read_clipboard_dib_bytes() -> Result<Vec<u8>, ClipboardError> {
    windows_clipboard::read_dib_bytes()
}

fn unique_clipboard_image_name() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();

    format!("splice-clipboard-{millis}.png")
}

pub fn sweep_temp_images(temp_dir: &Path) -> Result<(), ClipboardError> {
    let entries = match fs::read_dir(temp_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(ClipboardError::Io(e)),
    };

    let now = SystemTime::now();
    let five_minutes = Duration::from_secs(5 * 60);

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
            if file_name.starts_with("splice-clipboard-") && file_name.ends_with(".png") {
                if let Ok(metadata) = entry.metadata() {
                    let file_time = metadata
                        .modified()
                        .or_else(|_| metadata.created())
                        .unwrap_or(now);

                    if let Ok(duration) = now.duration_since(file_time) {
                        if duration > five_minutes {
                            let _ = fs::remove_file(path);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(windows)]
mod windows_clipboard {
    use super::ClipboardError;
    use std::{ffi::c_void, ptr::NonNull, slice, thread::sleep, time::Duration};
    use windows::Win32::{
        Foundation::{GlobalFree, HANDLE, HGLOBAL},
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
                OpenClipboard, SetClipboardData,
            },
            Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
            Ole::{CF_DIB, CF_DIBV5, CF_UNICODETEXT},
        },
    };

    // OpenClipboard fails with ERROR_ACCESS_DENIED while another process owns the
    // clipboard. Contention is real (clipboard managers, other terminals), so retry
    // a bounded number of times with a short backoff before giving up.
    const OPEN_RETRY_ATTEMPTS: u32 = 10;
    const OPEN_RETRY_DELAY: Duration = Duration::from_millis(10);

    // Write UTF-16 code units (NUL-terminated) to the clipboard as CF_UNICODETEXT.
    //
    // Handle-ownership contract: SetClipboardData transfers ownership of the
    // GlobalAlloc'd block to the system ON SUCCESS — after that the block must NOT
    // be freed here (the system frees it, and a GlobalFree would be a double-free).
    // The block is only freed on the error paths BEFORE ownership transfers.
    pub fn write_unicode_text(units: &[u16]) -> Result<(), ClipboardError> {
        let _clipboard = ClipboardGuard::open_with_retry()?;
        unsafe {
            EmptyClipboard()?;
        }

        let byte_len = std::mem::size_of_val(units);
        let hglobal = unsafe { GlobalAlloc(GMEM_MOVEABLE, byte_len)? };

        // Copy the UTF-16 buffer into the moveable block. Any failure here frees the
        // block ourselves, since ownership has NOT yet passed to the clipboard.
        if let Err(error) = fill_global(hglobal, units) {
            unsafe {
                let _ = GlobalFree(Some(hglobal));
            }
            return Err(error);
        }

        match unsafe { SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(hglobal.0))) } {
            // Ownership now belongs to the system; do NOT free the handle.
            Ok(_) => Ok(()),
            Err(error) => {
                unsafe {
                    let _ = GlobalFree(Some(hglobal));
                }
                Err(error.into())
            }
        }
    }

    fn fill_global(hglobal: HGLOBAL, units: &[u16]) -> Result<(), ClipboardError> {
        let ptr = unsafe { GlobalLock(hglobal) };
        if ptr.is_null() {
            return Err(ClipboardError::Windows(windows::core::Error::from_win32()));
        }

        unsafe {
            std::ptr::copy_nonoverlapping(units.as_ptr(), ptr.cast::<u16>(), units.len());
            // GlobalUnlock returns Err with NO_ERROR once the lock count reaches
            // zero, so its result is intentionally ignored.
            let _ = GlobalUnlock(hglobal);
        }

        Ok(())
    }

    pub fn read_dib_bytes() -> Result<Vec<u8>, ClipboardError> {
        let _clipboard = ClipboardGuard::open()?;
        let format = available_image_format()?;
        let handle = unsafe { GetClipboardData(format)? };
        let global = HGLOBAL(handle.0);
        let locked = GlobalLockGuard::lock(global)?;

        Ok(locked.bytes().to_vec())
    }

    // Read CF_UNICODETEXT and decode it to a String. Symmetric with
    // write_unicode_text: it reuses open_with_retry (the clipboard can be briefly
    // owned by another process) and GlobalLockGuard (RAII unlock). A missing text
    // format is NOT an error — it signals "no text to paste" as an empty String so
    // the caller can fall back to the image route.
    pub fn read_unicode_text() -> Result<String, ClipboardError> {
        let _clipboard = ClipboardGuard::open_with_retry()?;

        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32) }.is_err() {
            return Ok(String::new());
        }

        let handle = unsafe { GetClipboardData(CF_UNICODETEXT.0 as u32)? };
        let global = HGLOBAL(handle.0);
        let locked = GlobalLockGuard::lock(global)?;

        Ok(super::decode_clipboard_text_utf16(locked.utf16_units()))
    }

    fn available_image_format() -> Result<u32, ClipboardError> {
        // Prefer DIBV5 when available because it can preserve richer bitmap metadata
        // (including a real alpha channel from well-behaved apps). The zero-alpha
        // fixup in dib_to_png neutralizes DIBV5's all-zero-alpha hazard, so CF_DIBV5
        // stays the preferred format.
        if unsafe { IsClipboardFormatAvailable(CF_DIBV5.0 as u32) }.is_ok() {
            return Ok(CF_DIBV5.0 as u32);
        }

        if unsafe { IsClipboardFormatAvailable(CF_DIB.0 as u32) }.is_ok() {
            return Ok(CF_DIB.0 as u32);
        }

        Err(ClipboardError::EmptyClipboardImage)
    }

    struct ClipboardGuard;

    impl ClipboardGuard {
        fn open() -> Result<Self, ClipboardError> {
            unsafe {
                OpenClipboard(None)?;
            }

            Ok(Self)
        }

        fn open_with_retry() -> Result<Self, ClipboardError> {
            let mut last_error: Option<windows::core::Error> = None;
            for attempt in 0..OPEN_RETRY_ATTEMPTS {
                match unsafe { OpenClipboard(None) } {
                    Ok(()) => return Ok(Self),
                    Err(error) => {
                        last_error = Some(error);
                        if attempt + 1 < OPEN_RETRY_ATTEMPTS {
                            sleep(OPEN_RETRY_DELAY);
                        }
                    }
                }
            }

            Err(ClipboardError::Windows(
                last_error.unwrap_or_else(windows::core::Error::from_win32),
            ))
        }
    }

    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }

    struct GlobalLockGuard {
        handle: HGLOBAL,
        ptr: NonNull<c_void>,
        size: usize,
    }

    impl GlobalLockGuard {
        fn lock(handle: HGLOBAL) -> Result<Self, ClipboardError> {
            if handle.0.is_null() {
                return Err(ClipboardError::EmptyClipboardImage);
            }

            let size = unsafe { GlobalSize(handle) };
            if size == 0 {
                return Err(ClipboardError::EmptyClipboardImage);
            }

            let ptr = unsafe { GlobalLock(handle) };
            let ptr = NonNull::new(ptr).ok_or(ClipboardError::EmptyClipboardImage)?;

            Ok(Self { handle, ptr, size })
        }

        fn bytes(&self) -> &[u8] {
            unsafe { slice::from_raw_parts(self.ptr.as_ptr().cast::<u8>(), self.size) }
        }

        // View the locked block as UTF-16 code units for CF_UNICODETEXT. GlobalAlloc
        // memory is at least pointer-aligned, so the u16 cast is sound; a trailing
        // odd byte (size not a multiple of 2) is truncated by the integer division,
        // which cannot form a complete code unit anyway.
        fn utf16_units(&self) -> &[u16] {
            unsafe { slice::from_raw_parts(self.ptr.as_ptr().cast::<u16>(), self.size / 2) }
        }
    }

    impl Drop for GlobalLockGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = GlobalUnlock(self.handle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

    #[test]
    fn encodes_clipboard_text_as_nul_terminated_utf16() {
        // A bare "a" becomes the UTF-16 code units for 'a' plus a NUL terminator,
        // which CF_UNICODETEXT requires.
        assert_eq!(encode_clipboard_text_utf16("a"), vec![0x0061, 0x0000]);
    }

    #[test]
    fn normalizes_lone_newlines_to_crlf_before_encoding() {
        // Windows clipboard convention is CRLF line endings. "a\nb" must encode as
        // the UTF-16 units for "a\r\nb\0".
        assert_eq!(
            encode_clipboard_text_utf16("a\nb"),
            vec![0x0061, 0x000D, 0x000A, 0x0062, 0x0000]
        );
    }

    #[test]
    fn does_not_double_convert_existing_crlf() {
        // An input that already uses CRLF must not become CRCRLF.
        assert_eq!(
            encode_clipboard_text_utf16("a\r\nb"),
            vec![0x0061, 0x000D, 0x000A, 0x0062, 0x0000]
        );
    }

    #[test]
    fn encodes_astral_characters_as_surrogate_pairs() {
        // U+1F600 (😀) encodes to the surrogate pair D83D DE00, then the NUL.
        assert_eq!(
            encode_clipboard_text_utf16("\u{1F600}"),
            vec![0xD83D, 0xDE00, 0x0000]
        );
    }

    #[test]
    fn decodes_utf16_up_to_the_nul_terminator() {
        // CF_UNICODETEXT buffers are NUL-terminated; the decode must stop at the
        // first NUL and ignore any trailing allocation slack after it.
        let units = [0x0061u16, 0x0062, 0x0000, 0x00FF, 0x00FF];
        assert_eq!(decode_clipboard_text_utf16(&units), "ab");
    }

    #[test]
    fn decodes_empty_utf16_buffer_to_empty_string() {
        // A lone NUL (the empty-string clipboard payload) and a truly empty slice
        // both decode to "".
        assert_eq!(decode_clipboard_text_utf16(&[0x0000u16]), "");
        assert_eq!(decode_clipboard_text_utf16(&[]), "");
    }

    #[test]
    fn decodes_utf16_without_a_nul_terminator() {
        // A buffer with no NUL must decode all of its units rather than panic or
        // truncate.
        assert_eq!(decode_clipboard_text_utf16(&[0x0061u16, 0x0062]), "ab");
    }

    #[test]
    fn decodes_utf16_surrogate_pairs_before_the_nul() {
        // U+1F600 (😀) is the surrogate pair D83D DE00; it must round-trip through
        // the decode and stop at the following NUL.
        let units = [0xD83Du16, 0xDE00, 0x0000];
        assert_eq!(decode_clipboard_text_utf16(&units), "\u{1F600}");
    }

    #[test]
    fn round_trips_clipboard_encode_then_decode() {
        // The write path CRLF-normalizes and NUL-terminates; decoding that exact
        // buffer must recover the CRLF-normalized text (the shape the clipboard
        // actually holds).
        let encoded = encode_clipboard_text_utf16("line1\nline2");
        assert_eq!(decode_clipboard_text_utf16(&encoded), "line1\r\nline2");
    }

    #[test]
    fn only_image_payload_prefers_image_pipeline() {
        assert!(!ClipboardPayload::Empty.prefers_image());
        assert!(!ClipboardPayload::Text("hello".to_owned()).prefers_image());
        assert!(ClipboardPayload::Image(ImagePaste {
            path: "C:/Temp/splice/image.png".to_owned(),
            mime_type: "image/png".to_owned(),
        })
        .prefers_image());
    }

    #[test]
    fn converts_zero_alpha_dibv5_to_opaque_png_and_flips_bottom_up_rows() {
        let png = dib_to_png(&money_dib()).expect("valid DIBV5 should convert to PNG");

        assert_eq!(&png[0..8], &PNG_SIGNATURE, "output must be a real PNG");

        let image = decode_png(&png);
        assert_eq!(image.dimensions(), (2, 2));

        // The LAST DIB row holds the top image row (bottom-up storage). Its first
        // pixel is red, so decoded pixel (0,0) must be red — proving the row flip.
        let top_left = image.get_pixel(0, 0);
        assert_eq!(
            top_left.0,
            [255, 0, 0, 255],
            "top-left must be opaque red (zero-alpha fixup + bottom-up flip)"
        );
    }

    #[test]
    fn preserves_genuine_alpha_when_any_pixel_is_transparent() {
        let png = dib_to_png(&money_dib_with_alpha(128)).expect("DIBV5 with alpha should convert");

        let image = decode_png(&png);
        let top_left = image.get_pixel(0, 0);
        assert_eq!(
            top_left.0,
            [255, 0, 0, 128],
            "genuine alpha must survive (fixup must not clobber it)"
        );
    }

    #[test]
    fn honors_top_down_dib_row_order() {
        // Same pixels as money_dib but with a negative height (top-down). Now the
        // FIRST DIB row is the top image row, whose first pixel is blue.
        let png = dib_to_png(&top_down_dib()).expect("top-down DIB should convert");

        let image = decode_png(&png);
        let top_left = image.get_pixel(0, 0);
        assert_eq!(
            top_left.0,
            [0, 0, 255, 255],
            "top-left must be opaque blue (first DIB row = top row)"
        );
    }

    #[test]
    fn decodes_short_layout_v5_bitfields_without_skipping_pixels() {
        // Short-layout CF_DIBV5 (Paint.NET / mmozeiko style): masks live only in the
        // header, there is NO 12-byte trailer, and pixels start immediately at byte
        // 124. The decoder must NOT skip 12 bytes here or it reads garbage/EOF.
        let png = dib_to_png(&short_layout_v5_bitfields_dib())
            .expect("short-layout DIBV5 should convert");

        assert_eq!(&png[0..8], &PNG_SIGNATURE, "output must be a real PNG");

        let image = decode_png(&png);
        assert_eq!(image.dimensions(), (2, 2));
        assert_eq!(image.get_pixel(0, 0).0, [255, 0, 0, 255], "top-left red");
        assert_eq!(image.get_pixel(1, 0).0, [0, 255, 0, 255], "top-right green");
        assert_eq!(
            image.get_pixel(0, 1).0,
            [0, 0, 255, 255],
            "bottom-left blue"
        );
        assert_eq!(
            image.get_pixel(1, 1).0,
            [255, 255, 255, 255],
            "bottom-right white"
        );
    }

    #[test]
    fn decodes_v5_bi_rgb_dib_at_header_end() {
        // A 124-byte BITMAPV5HEADER with BI_RGB (no bitfields, no mask trailer):
        // pixels start immediately at byte 124. The 4th byte per pixel is padding.
        let png = dib_to_png(&v5_bi_rgb_dib()).expect("V5 BI_RGB DIB should convert");

        let image = decode_png(&png);
        assert_eq!(image.dimensions(), (2, 2));
        assert_eq!(image.get_pixel(0, 0).0, [255, 0, 0, 255], "top-left red");
        assert_eq!(image.get_pixel(1, 0).0, [0, 255, 0, 255], "top-right green");
        assert_eq!(
            image.get_pixel(0, 1).0,
            [0, 0, 255, 255],
            "bottom-left blue"
        );
        assert_eq!(
            image.get_pixel(1, 1).0,
            [255, 255, 255, 255],
            "bottom-right white"
        );
    }

    #[test]
    fn decodes_24_bit_dib_respecting_row_stride_padding() {
        let png = dib_to_png(&three_pixel_24_bit_dib()).expect("24-bit DIB should convert");

        let image = decode_png(&png);
        assert_eq!(image.dimensions(), (3, 1));
        assert_eq!(image.get_pixel(0, 0).0, [255, 0, 0, 255], "pixel 0 is red");
        assert_eq!(
            image.get_pixel(1, 0).0,
            [0, 255, 0, 255],
            "pixel 1 is green"
        );
        assert_eq!(image.get_pixel(2, 0).0, [0, 0, 255, 255], "pixel 2 is blue");
    }

    #[test]
    fn treats_32_bit_bi_rgb_fourth_byte_as_padding() {
        let png = dib_to_png(&single_pixel_32_bit_bi_rgb_dib(0xAB))
            .expect("32bpp BI_RGB DIB should convert");

        let image = decode_png(&png);
        assert_eq!(image.dimensions(), (1, 1));
        assert_eq!(
            image.get_pixel(0, 0).0,
            [255, 0, 0, 255],
            "opaque red; junk 4th byte must be ignored"
        );
    }

    #[test]
    fn rejects_garbage_dib_without_writing_a_file() {
        let temp_dir = fresh_temp_dir("garbage");

        let error = persist_dib_as_png(&temp_dir, &[40, 0, 0])
            .expect_err("garbage DIB must not persist a file");

        assert!(matches!(error, ClipboardError::Image(_)));
        assert!(
            !temp_dir.exists(),
            "no file (or directory) should be created on decode failure"
        );
    }

    #[test]
    fn surface_uses_png_extension_and_mime() {
        assert!(unique_clipboard_image_name().ends_with(".png"));

        let temp_dir = fresh_temp_dir("surface");
        let paste =
            image_paste_from_dib(&temp_dir, &money_dib()).expect("valid DIB should build a paste");

        assert!(paste.path.ends_with(".png"), "persisted path must be .png");
        assert_eq!(paste.mime_type, "image/png");

        fs::remove_dir_all(&temp_dir).expect("temp directory should be cleaned");
    }

    #[test]
    fn decodes_8bpp_indexed_dib_with_default_full_palette() {
        // 8bpp BI_RGB indexed DIB with clr_used == 0, which means "full 2^8 palette"
        // (256 entries, 1024 bytes). The pixel offset MUST skip the whole palette; a
        // naive clr_used*4 == 0 lands the offset inside the palette and decodes garbage.
        let png = dib_to_png(&indexed_8bpp_dib()).expect("8bpp indexed DIB should convert");

        assert_eq!(&png[0..8], &PNG_SIGNATURE, "output must be a real PNG");

        let image = decode_png(&png);
        assert_eq!(image.dimensions(), (2, 1));
        assert_eq!(
            image.get_pixel(0, 0).0,
            [255, 0, 0, 255],
            "index 0 -> opaque red"
        );
        assert_eq!(
            image.get_pixel(1, 0).0,
            [0, 255, 0, 255],
            "index 1 -> opaque green"
        );
    }

    #[test]
    fn masks_check_classifies_short_layout_despite_long_eligible_size() {
        // Adversarial short-layout V5 BITFIELDS: it carries 12 bytes of trailing slack
        // (models GlobalSize over-reporting allocation granularity), so the size guard
        // alone would deem it "long" and skip a 12-byte trailer. But the first 12 pixel
        // bytes do NOT equal the header R/G/B masks, so the masks_match check must still
        // classify it SHORT and decode the real pixels. This is the test that actually
        // exercises masks_match.
        let png = dib_to_png(&short_layout_v5_bitfields_dib_with_trailing_slack())
            .expect("short-layout DIBV5 with slack should convert");

        let image = decode_png(&png);
        assert_eq!(image.dimensions(), (2, 2));
        assert_eq!(image.get_pixel(0, 0).0, [255, 0, 0, 255], "top-left red");
        assert_eq!(image.get_pixel(1, 0).0, [0, 255, 0, 255], "top-right green");
        assert_eq!(
            image.get_pixel(0, 1).0,
            [0, 0, 255, 255],
            "bottom-left blue"
        );
        assert_eq!(
            image.get_pixel(1, 1).0,
            [255, 255, 255, 255],
            "bottom-right white"
        );
    }

    fn decode_png(bytes: &[u8]) -> image::RgbaImage {
        image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
            .expect("clipboard output should decode as PNG")
            .to_rgba8()
    }

    fn fresh_temp_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "splice-clipboard-{tag}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after epoch")
                .as_nanos()
        ))
    }

    // BITMAPV5HEADER (124 bytes), 32bpp, BI_BITFIELDS with standard BGRA masks.
    // `long` selects the pixel layout: when true a redundant 12-byte RGB mask
    // trailer is appended after the header (Windows-synthesized CF_DIBV5, pixels
    // at byte 136); when false the pixels start immediately after the 124-byte
    // header (short layout written by apps like Paint.NET, pixels at byte 124).
    fn v5_bitfields_header(width: i32, height: i32, long: bool) -> Vec<u8> {
        let stride = width.unsigned_abs() * 4;
        let size_image = stride * height.unsigned_abs();

        let mut dib = Vec::with_capacity(124);
        dib.extend_from_slice(&124u32.to_le_bytes()); // bV5Size
        dib.extend_from_slice(&width.to_le_bytes()); // bV5Width
        dib.extend_from_slice(&height.to_le_bytes()); // bV5Height (negative = top-down)
        dib.extend_from_slice(&1u16.to_le_bytes()); // bV5Planes
        dib.extend_from_slice(&32u16.to_le_bytes()); // bV5BitCount
        dib.extend_from_slice(&3u32.to_le_bytes()); // bV5Compression = BI_BITFIELDS
        dib.extend_from_slice(&size_image.to_le_bytes()); // bV5SizeImage
        dib.extend_from_slice(&0i32.to_le_bytes()); // bV5XPelsPerMeter
        dib.extend_from_slice(&0i32.to_le_bytes()); // bV5YPelsPerMeter
        dib.extend_from_slice(&0u32.to_le_bytes()); // bV5ClrUsed
        dib.extend_from_slice(&0u32.to_le_bytes()); // bV5ClrImportant
        dib.extend_from_slice(&0x00FF_0000u32.to_le_bytes()); // bV5RedMask
        dib.extend_from_slice(&0x0000_FF00u32.to_le_bytes()); // bV5GreenMask
        dib.extend_from_slice(&0x0000_00FFu32.to_le_bytes()); // bV5BlueMask
        dib.extend_from_slice(&0xFF00_0000u32.to_le_bytes()); // bV5AlphaMask
        dib.resize(124, 0); // remaining V5 fields (color space, gamma, intent, profile)

        if long {
            // Windows-synthesized (long) layout: the RGB color masks are duplicated
            // in a 12-byte trailer after the header, and the pixel data follows it.
            dib.extend_from_slice(&0x00FF_0000u32.to_le_bytes()); // red mask (trailer)
            dib.extend_from_slice(&0x0000_FF00u32.to_le_bytes()); // green mask (trailer)
            dib.extend_from_slice(&0x0000_00FFu32.to_le_bytes()); // blue mask (trailer)
        }
        dib
    }

    // 2x2 image with a distinct color per corner, all alpha bytes 0, bottom-up.
    // Stored order (bottom row first): [blue, white] then [red, green].
    fn money_dib() -> Vec<u8> {
        let mut dib = v5_bitfields_header(2, 2, true);
        dib.extend_from_slice(&[
            255, 0, 0, 0, // (0,1) bottom-left blue  (B,G,R,A)
            255, 255, 255, 0, // (1,1) bottom-right white
            0, 0, 255, 0, // (0,0) top-left red
            0, 255, 0, 0, // (1,0) top-right green
        ]);
        dib
    }

    // Same as money_dib but the top-left (red) pixel carries a real alpha value.
    fn money_dib_with_alpha(alpha: u8) -> Vec<u8> {
        let mut dib = v5_bitfields_header(2, 2, true);
        dib.extend_from_slice(&[
            255, 0, 0, 0, // bottom-left blue
            255, 255, 255, 0, // bottom-right white
            0, 0, 255, alpha, // top-left red with genuine alpha
            0, 255, 0, 0, // top-right green
        ]);
        dib
    }

    // 2x2 short-layout BITFIELDS DIBV5: pixels start at byte 124 (no mask trailer).
    // Distinct opaque colors per corner, bottom-up storage (bottom row stored first).
    fn short_layout_v5_bitfields_dib() -> Vec<u8> {
        let mut dib = v5_bitfields_header(2, 2, false);
        // Bottom-up storage: the bottom image row is stored first. Bytes are B,G,R,A.
        dib.extend_from_slice(&[
            255, 0, 0, 255, // (0,1) bottom-left blue
            255, 255, 255, 255, // (1,1) bottom-right white
            0, 0, 255, 255, // (0,0) top-left red
            0, 255, 0, 255, // (1,0) top-right green
        ]);
        dib
    }

    // 2x2 BITMAPV5HEADER, 32bpp, BI_RGB (compression 0): no masks, no trailer,
    // pixels start at byte 124. Bottom-up storage; the 4th byte is padding.
    fn v5_bi_rgb_dib() -> Vec<u8> {
        let width = 2i32;
        let height = 2i32;
        let stride = width.unsigned_abs() * 4;
        let size_image = stride * height.unsigned_abs();

        let mut dib = Vec::with_capacity(124);
        dib.extend_from_slice(&124u32.to_le_bytes()); // bV5Size
        dib.extend_from_slice(&width.to_le_bytes()); // bV5Width
        dib.extend_from_slice(&height.to_le_bytes()); // bV5Height
        dib.extend_from_slice(&1u16.to_le_bytes()); // bV5Planes
        dib.extend_from_slice(&32u16.to_le_bytes()); // bV5BitCount
        dib.extend_from_slice(&0u32.to_le_bytes()); // bV5Compression = BI_RGB
        dib.extend_from_slice(&size_image.to_le_bytes()); // bV5SizeImage
        dib.extend_from_slice(&0i32.to_le_bytes()); // bV5XPelsPerMeter
        dib.extend_from_slice(&0i32.to_le_bytes()); // bV5YPelsPerMeter
        dib.extend_from_slice(&0u32.to_le_bytes()); // bV5ClrUsed
        dib.extend_from_slice(&0u32.to_le_bytes()); // bV5ClrImportant
        dib.resize(124, 0); // masks + remaining V5 fields stay zero for BI_RGB

        // Bottom-up: bottom image row stored first. Bytes are B,G,R + padding.
        dib.extend_from_slice(&[
            255, 0, 0, 0, // (0,1) bottom-left blue
            255, 255, 255, 0, // (1,1) bottom-right white
            0, 0, 255, 0, // (0,0) top-left red
            0, 255, 0, 0, // (1,0) top-right green
        ]);
        dib
    }

    // Same pixel bytes as money_dib but top-down (negative height): the first
    // stored row becomes the top image row.
    fn top_down_dib() -> Vec<u8> {
        let mut dib = v5_bitfields_header(2, -2, true);
        // Identical pixel bytes to money_dib; only the height sign differs. With a
        // top-down DIB the FIRST stored row is the top image row, so pixel (0,0)
        // becomes blue here (vs. red in the bottom-up money_dib) — proving row order.
        dib.extend_from_slice(&[
            255, 0, 0, 0, // first stored row -> top: (0,0) blue
            255, 255, 255, 0, // (1,0) white
            0, 0, 255, 0, // second stored row -> bottom: (0,1) red
            0, 255, 0, 0, // (1,1) green
        ]);
        dib
    }

    // BITMAPINFOHEADER (40 bytes).
    fn info_header(
        width: i32,
        height: i32,
        bit_count: u16,
        compression: u32,
        size_image: u32,
    ) -> Vec<u8> {
        let mut dib = Vec::with_capacity(40);
        dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
        dib.extend_from_slice(&width.to_le_bytes());
        dib.extend_from_slice(&height.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        dib.extend_from_slice(&bit_count.to_le_bytes());
        dib.extend_from_slice(&compression.to_le_bytes());
        dib.extend_from_slice(&size_image.to_le_bytes());
        dib.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
        dib.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
        dib.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
        dib.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
        dib
    }

    // 3x1 24-bit BI_RGB image. Row is 9 data bytes padded to a 12-byte stride.
    fn three_pixel_24_bit_dib() -> Vec<u8> {
        let mut dib = info_header(3, 1, 24, 0, 12);
        dib.extend_from_slice(&[
            0, 0, 255, // red   (B,G,R)
            0, 255, 0, // green
            255, 0, 0, // blue
            0, 0, 0, // stride padding to 12 bytes
        ]);
        dib
    }

    // 1x1 32-bit BI_RGB image; the 4th byte is junk and must be ignored.
    fn single_pixel_32_bit_bi_rgb_dib(junk: u8) -> Vec<u8> {
        let mut dib = info_header(1, 1, 32, 0, 4);
        dib.extend_from_slice(&[0, 0, 255, junk]); // red (B,G,R) + junk
        dib
    }

    // 2x1 8bpp BI_RGB indexed DIB with clr_used == 0 (the "full 256-entry palette"
    // convention). Palette index 0 = red, index 1 = green; the index row is [0, 1]
    // padded to the 4-byte DWORD stride. The pixels only decode correctly if the whole
    // 1024-byte palette is skipped.
    fn indexed_8bpp_dib() -> Vec<u8> {
        // Row stride is ((2*8 + 31) / 32) * 4 = 4 bytes for the single row.
        let mut dib = info_header(2, 1, 8, 0, 4);

        // 256-entry RGBQUAD palette (B,G,R,0). Only indices 0 and 1 are meaningful.
        let mut palette = vec![0u8; 256 * 4];
        palette[0..4].copy_from_slice(&[0, 0, 255, 0]); // index 0 -> red
        palette[4..8].copy_from_slice(&[0, 255, 0, 0]); // index 1 -> green
        dib.extend_from_slice(&palette);

        // 2x1 index row [0, 1] padded to the 4-byte DWORD stride.
        dib.extend_from_slice(&[0, 1, 0, 0]);
        dib
    }

    // Short-layout V5 BITFIELDS whose byte length is large enough that the size guard
    // alone would treat it as "long" (>= 12-byte trailer + full pixel buffer). The 12
    // bytes of trailing slack after the 16-byte pixel buffer model GlobalSize
    // over-reporting allocation granularity. The first 12 pixel bytes deliberately do
    // NOT match the header R/G/B masks, so only the masks_match check keeps the layout
    // classified as SHORT.
    fn short_layout_v5_bitfields_dib_with_trailing_slack() -> Vec<u8> {
        let mut dib = v5_bitfields_header(2, 2, false);
        // Bottom-up storage, bytes B,G,R,A. Opaque alpha (255) guarantees each pixel's
        // little-endian u32 has 0xFF in its high byte, so it can never equal a header
        // R/G/B mask (which all have a 0x00 high byte).
        dib.extend_from_slice(&[
            255, 0, 0, 255, // (0,1) bottom-left blue
            255, 255, 255, 255, // (1,1) bottom-right white
            0, 0, 255, 255, // (0,0) top-left red
            0, 255, 0, 255, // (1,0) top-right green
        ]);
        // 12 bytes of trailing slack: enough that (dib.len - 124) >= 12 + 16, so the
        // size guard alone would consider a long-layout trailer plausible. masks_match
        // must override that and keep the layout SHORT.
        dib.extend_from_slice(&[0u8; 12]);
        dib
    }

    fn set_file_modified_time(path: &std::path::Path, time: SystemTime) -> std::io::Result<()> {
        let file = fs::OpenOptions::new().write(true).open(path)?;
        let mut times = std::fs::FileTimes::new();
        times = times.set_modified(time);
        times = times.set_accessed(time);
        file.set_times(times)?;
        Ok(())
    }

    #[test]
    fn test_sweep_temp_images_deletes_old_files() {
        let temp_dir = fresh_temp_dir("sweep");
        fs::create_dir_all(&temp_dir).unwrap();

        let now = SystemTime::now();

        // 1. Create an old matching file (10 minutes old)
        let old_matching_path = temp_dir.join("splice-clipboard-111.png");
        fs::write(&old_matching_path, b"dummy png content").unwrap();
        set_file_modified_time(&old_matching_path, now - Duration::from_secs(600)).unwrap();

        // 2. Create a new matching file (2 minutes old)
        let new_matching_path = temp_dir.join("splice-clipboard-222.png");
        fs::write(&new_matching_path, b"dummy png content").unwrap();
        set_file_modified_time(&new_matching_path, now - Duration::from_secs(120)).unwrap();

        // 3. Create an old non-matching file (10 minutes old, wrong prefix)
        let old_non_matching_path = temp_dir.join("other-file-333.png");
        fs::write(&old_non_matching_path, b"dummy png content").unwrap();
        set_file_modified_time(&old_non_matching_path, now - Duration::from_secs(600)).unwrap();

        // 4. Create an old non-matching file (10 minutes old, wrong extension)
        let old_wrong_ext_path = temp_dir.join("splice-clipboard-444.txt");
        fs::write(&old_wrong_ext_path, b"dummy txt content").unwrap();
        set_file_modified_time(&old_wrong_ext_path, now - Duration::from_secs(600)).unwrap();

        // Run the sweeper
        sweep_temp_images(&temp_dir).unwrap();

        // Assertions:
        // Old matching file should be deleted
        assert!(
            !old_matching_path.exists(),
            "Old matching file should be deleted"
        );

        // New matching file should still exist
        assert!(
            new_matching_path.exists(),
            "New matching file should be preserved"
        );

        // Old non-matching files should still exist
        assert!(
            old_non_matching_path.exists(),
            "Old non-matching file should be preserved"
        );
        assert!(
            old_wrong_ext_path.exists(),
            "Old file with wrong extension should be preserved"
        );

        // Cleanup
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_sweep_temp_images_handles_non_existent_directory() {
        let temp_dir = fresh_temp_dir("non-existent");
        // Do not create the directory
        let result = sweep_temp_images(&temp_dir);
        assert!(
            result.is_ok(),
            "Sweeping a non-existent directory should succeed with Ok(())"
        );
    }
}
