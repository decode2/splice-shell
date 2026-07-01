use splice_core::{ImagePaste, PastePayload};
use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
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
    InvalidDib(&'static str),
    UnsupportedPlatform,
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(windows)]
            Self::Windows(error) => write!(f, "Windows clipboard error: {error}"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::EmptyClipboardImage => write!(f, "clipboard does not contain image data"),
            Self::InvalidDib(reason) => write!(f, "invalid DIB image: {reason}"),
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
    let path = persist_dib_as_bmp(temp_dir, &dib)?;

    Ok(ClipboardPayload::Image(ImagePaste {
        path: path.to_string_lossy().into_owned(),
        mime_type: "image/bmp".to_owned(),
    }))
}

pub fn read_clipboard_image_paste_payload(temp_dir: &Path) -> Result<PastePayload, ClipboardError> {
    match read_clipboard_image_to_temp_dir(temp_dir)? {
        ClipboardPayload::Image(image) => Ok(PastePayload::Image(image)),
        ClipboardPayload::Text(text) => Ok(PastePayload::Text(text)),
        ClipboardPayload::Empty => Err(ClipboardError::EmptyClipboardImage),
    }
}

fn persist_dib_as_bmp(temp_dir: &Path, dib: &[u8]) -> Result<std::path::PathBuf, ClipboardError> {
    let bmp = dib_to_bmp(dib)?;
    fs::create_dir_all(temp_dir)?;

    let path = temp_dir.join(unique_clipboard_image_name());
    fs::write(&path, bmp)?;
    Ok(path)
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

    format!("splice-clipboard-{millis}.bmp")
}

fn dib_to_bmp(dib: &[u8]) -> Result<Vec<u8>, ClipboardError> {
    let pixel_offset = dib_pixel_offset(dib)?;
    let file_size = 14usize
        .checked_add(dib.len())
        .ok_or(ClipboardError::InvalidDib("BMP file size overflow"))?;
    let pixel_offset = 14usize
        .checked_add(pixel_offset)
        .ok_or(ClipboardError::InvalidDib("BMP pixel offset overflow"))?;

    let mut bmp = Vec::with_capacity(file_size);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(file_size as u32).to_le_bytes());
    bmp.extend_from_slice(&0u16.to_le_bytes());
    bmp.extend_from_slice(&0u16.to_le_bytes());
    bmp.extend_from_slice(&(pixel_offset as u32).to_le_bytes());
    bmp.extend_from_slice(dib);

    Ok(bmp)
}

fn dib_pixel_offset(dib: &[u8]) -> Result<usize, ClipboardError> {
    if dib.len() < 16 {
        return Err(ClipboardError::InvalidDib("DIB is too small"));
    }

    let header_size = read_u32_le(dib, 0)? as usize;
    if header_size < 12 {
        return Err(ClipboardError::InvalidDib("unsupported DIB header size"));
    }
    if dib.len() < header_size {
        return Err(ClipboardError::InvalidDib(
            "DIB shorter than declared header",
        ));
    }

    if header_size == 12 {
        let bit_count = read_u16_le(dib, 10)?;
        let colors = if bit_count <= 8 {
            1usize << bit_count
        } else {
            0
        };
        return header_size
            .checked_add(colors * 3)
            .ok_or(ClipboardError::InvalidDib("DIB color table overflow"));
    }

    if dib.len() < 40 {
        return Err(ClipboardError::InvalidDib("BITMAPINFOHEADER is incomplete"));
    }

    let bit_count = read_u16_le(dib, 14)?;
    let compression = read_u32_le(dib, 16)?;
    let colors_used = read_u32_le(dib, 32)? as usize;
    let colors = if colors_used > 0 {
        colors_used
    } else if bit_count <= 8 {
        1usize << bit_count
    } else {
        0
    };
    let bitfields_bytes = if header_size == 40 && compression == 3 {
        12usize
    } else {
        0usize
    };

    header_size
        .checked_add(bitfields_bytes)
        .and_then(|offset| offset.checked_add(colors * 4))
        .ok_or(ClipboardError::InvalidDib("DIB pixel offset overflow"))
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Result<u16, ClipboardError> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or(ClipboardError::InvalidDib("unexpected end of DIB"))?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, ClipboardError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or(ClipboardError::InvalidDib("unexpected end of DIB"))?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[cfg(windows)]
mod windows_clipboard {
    use super::ClipboardError;
    use std::{ffi::c_void, ptr::NonNull, slice};
    use windows::Win32::{
        Foundation::HGLOBAL,
        System::{
            DataExchange::{
                CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
            },
            Memory::{GlobalLock, GlobalSize, GlobalUnlock},
            Ole::{CF_DIB, CF_DIBV5},
        },
    };

    pub fn read_dib_bytes() -> Result<Vec<u8>, ClipboardError> {
        let _clipboard = ClipboardGuard::open()?;
        let format = available_image_format()?;
        let handle = unsafe { GetClipboardData(format)? };
        let global = HGLOBAL(handle.0);
        let locked = GlobalLockGuard::lock(global)?;

        Ok(locked.bytes().to_vec())
    }

    fn available_image_format() -> Result<u32, ClipboardError> {
        // Prefer DIBV5 when available because it can preserve richer bitmap metadata.
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
    fn dib_to_bmp_adds_file_header_without_copying_external_handles() {
        let dib = minimal_24_bit_dib();
        let bmp = dib_to_bmp(&dib).expect("valid DIB should convert to BMP");

        assert_eq!(&bmp[0..2], b"BM");
        assert_eq!(
            u32::from_le_bytes([bmp[2], bmp[3], bmp[4], bmp[5]]) as usize,
            bmp.len()
        );
        assert_eq!(u32::from_le_bytes([bmp[10], bmp[11], bmp[12], bmp[13]]), 54);
        assert_eq!(&bmp[14..], dib.as_slice());
    }

    #[test]
    fn dib_to_bmp_rejects_truncated_headers() {
        let error = dib_to_bmp(&[40, 0, 0]).expect_err("truncated DIB should fail");

        assert!(matches!(error, ClipboardError::InvalidDib(_)));
    }

    #[test]
    fn persist_dib_as_bmp_writes_file_and_releases_file_handle() {
        let temp_dir = std::env::temp_dir().join(format!(
            "splice-clipboard-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after epoch")
                .as_nanos()
        ));

        let path = persist_dib_as_bmp(&temp_dir, &minimal_24_bit_dib())
            .expect("DIB should persist as BMP");
        let bytes = fs::read(&path).expect("BMP should be readable");
        assert_eq!(&bytes[0..2], b"BM");

        fs::remove_dir_all(&temp_dir).expect("temp directory should be cleaned");
    }

    #[test]
    fn image_clipboard_payload_maps_to_core_paste_payload() {
        let temp_dir = std::env::temp_dir().join(format!(
            "splice-clipboard-payload-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after epoch")
                .as_nanos()
        ));

        let path =
            persist_dib_as_bmp(&temp_dir, &minimal_24_bit_dib()).expect("DIB should persist");
        let payload = ClipboardPayload::Image(ImagePaste {
            path: path.to_string_lossy().into_owned(),
            mime_type: "image/bmp".to_owned(),
        });

        let mapped = match payload {
            ClipboardPayload::Image(image) => PastePayload::Image(image),
            ClipboardPayload::Text(text) => PastePayload::Text(text),
            ClipboardPayload::Empty => panic!("test payload should not be empty"),
        };

        assert!(
            matches!(mapped, PastePayload::Image(ImagePaste { mime_type, .. }) if mime_type == "image/bmp")
        );

        fs::remove_dir_all(&temp_dir).expect("temp directory should be cleaned");
    }

    fn minimal_24_bit_dib() -> Vec<u8> {
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER size
        dib.extend_from_slice(&1i32.to_le_bytes()); // width
        dib.extend_from_slice(&1i32.to_le_bytes()); // height
        dib.extend_from_slice(&1u16.to_le_bytes()); // planes
        dib.extend_from_slice(&24u16.to_le_bytes()); // bit count
        dib.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        dib.extend_from_slice(&4u32.to_le_bytes()); // image size, padded row
        dib.extend_from_slice(&0i32.to_le_bytes()); // x pixels per meter
        dib.extend_from_slice(&0i32.to_le_bytes()); // y pixels per meter
        dib.extend_from_slice(&0u32.to_le_bytes()); // colors used
        dib.extend_from_slice(&0u32.to_le_bytes()); // important colors
        dib.extend_from_slice(&[0, 0, 255, 0]); // one red pixel in BGR + row padding
        dib
    }
}
