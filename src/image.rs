//! Screenshot validation. Only raster formats are accepted - never SVG or
//! anything else a browser could execute - and the format is detected from
//! the file contents, not from the client supplied name or content type.

use imagesize::ImageType;

/// Upper bound per side; real screenshots are far below this.
const MAX_DIMENSION: usize = 65_535;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl Format {
    pub fn ext(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpg",
            Format::Gif => "gif",
            Format::Webp => "webp",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Format::Png => "image/png",
            Format::Jpeg => "image/jpeg",
            Format::Gif => "image/gif",
            Format::Webp => "image/webp",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    pub format: Format,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImageError {
    #[error("file is empty")]
    Empty,
    #[error("unsupported file type, only PNG, JPEG, GIF and WebP screenshots are accepted")]
    Unsupported,
    #[error("image header is corrupt")]
    Corrupt,
    #[error("image dimensions are invalid")]
    BadDimensions,
}

pub fn inspect(data: &[u8]) -> Result<ImageInfo, ImageError> {
    if data.is_empty() {
        return Err(ImageError::Empty);
    }
    let format = match imagesize::image_type(data).map_err(|_| ImageError::Unsupported)? {
        ImageType::Png => Format::Png,
        ImageType::Jpeg => Format::Jpeg,
        ImageType::Gif => Format::Gif,
        ImageType::Webp => Format::Webp,
        _ => return Err(ImageError::Unsupported),
    };
    let size = imagesize::blob_size(data).map_err(|_| ImageError::Corrupt)?;
    let valid = |v: usize| (1..=MAX_DIMENSION).contains(&v);
    if !valid(size.width) || !valid(size.height) {
        return Err(ImageError::BadDimensions);
    }
    Ok(ImageInfo {
        format,
        width: size.width as u32,
        height: size.height as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid 1x1 PNG.
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00,
        0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00,
        0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05,
        0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42,
        0x60, 0x82,
    ];

    #[test]
    fn accepts_png() {
        let info = inspect(PNG_1X1).unwrap();
        assert_eq!(info.format, Format::Png);
        assert_eq!((info.width, info.height), (1, 1));
    }

    #[test]
    fn rejects_non_images() {
        assert_eq!(inspect(b""), Err(ImageError::Empty));
        assert_eq!(
            inspect(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
            Err(ImageError::Unsupported)
        );
        assert_eq!(
            inspect(b"hello world, definitely not an image"),
            Err(ImageError::Unsupported)
        );
    }
}
