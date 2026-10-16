//! Photo overlay compositing (FR-23).
//!
//! The photo is decoded, turned upright by its EXIF orientation, and the
//! overlay PNG is resized to the upright size (§4.4's 2% aspect rule applies
//! here too: fit to the width and centre vertically when the aspects
//! differ) and alpha-composited on top. JPEG sources give JPEG at quality 92
//! by default; PNG sources stay PNG, keeping their alpha channel.
//!
//! Nothing else is carried over: the output has no EXIF at all, so its
//! pixels are upright and must stay without an orientation tag. The engine
//! writes the capture metadata afterwards (§4.3).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use image::imageops::{self, FilterType};
use image::{DynamicImage, ImageEncoder, RgbImage, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::probe::{Container, probe};
use crate::video::{Placement, Warning, aspect_difference, checked_placement};

/// Photo compositing settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhotoOptions {
    /// JPEG quality, 1 to 100 (FR-23: 92).
    pub jpeg_quality: u8,
    /// §4.4 aspect tolerance, shared with video.
    pub aspect_tolerance: f64,
}

impl Default for PhotoOptions {
    fn default() -> Self {
        Self {
            jpeg_quality: 92,
            aspect_tolerance: 0.02,
        }
    }
}

/// Output encoding, decided by the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PhotoFormat {
    Jpeg,
    Png,
}

impl PhotoFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            PhotoFormat::Jpeg => "jpg",
            PhotoFormat::Png => "png",
        }
    }
}

/// What [`composite_photo`] did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhotoOutcome {
    pub format: PhotoFormat,
    /// Upright output size.
    pub size: (u32, u32),
    /// The EXIF orientation that was applied to the pixels (1 = none).
    pub orientation: u8,
    pub placement: Placement,
    pub warnings: Vec<Warning>,
}

/// Composite `overlay` onto `main` and write the result to `out`.
///
/// The output is written to a temporary file next to `out` and renamed into
/// place, so a crash never leaves a half-written photo under the final name.
/// The encoding follows the source ([`PhotoOutcome::format`]), whatever
/// `out`'s extension says; the engine names the file.
pub fn composite_photo(
    main: &Path,
    overlay: &Path,
    out: &Path,
    opts: &PhotoOptions,
) -> Result<PhotoOutcome, Error> {
    let info = probe(main)?;
    let (format, orientation) = match (&info.container, &info.image) {
        (Container::Jpeg, Some(i)) => (PhotoFormat::Jpeg, i.orientation),
        (Container::Png, Some(i)) => (PhotoFormat::Png, i.orientation),
        _ => {
            return Err(Error::Unsupported(format!(
                "{}: not a JPEG or PNG photo",
                main.display()
            )));
        }
    };
    let photo = decode(main, "photo")?;
    let photo = orient(photo, orientation);
    let size = (photo.width(), photo.height());

    let ov = decode(overlay, "overlay")?.into_rgba8();
    let placement = checked_placement(size, ov.dimensions(), opts.aspect_tolerance)?;
    let mut warnings = Vec::new();
    if placement.aspect_mismatch {
        let w = Warning::OverlayAspectMismatch {
            overlay: ov.dimensions(),
            target: size,
            difference: aspect_difference(size, ov.dimensions()),
        };
        w.log();
        warnings.push(w);
    }
    let ov = if ov.dimensions() == (placement.width, placement.height) {
        ov
    } else {
        imageops::resize(&ov, placement.width, placement.height, FilterType::Triangle)
    };

    let tmp = partial_path(out);
    let written = match format {
        PhotoFormat::Jpeg => {
            let mut base = photo.into_rgb8();
            blend_rgb(&mut base, &ov, placement.x, placement.y);
            write_jpeg(&tmp, &base, opts.jpeg_quality)
        }
        PhotoFormat::Png => {
            let has_alpha = photo.color().has_alpha();
            let mut base = photo.into_rgba8();
            blend_rgba(&mut base, &ov, placement.x, placement.y);
            if has_alpha {
                write_png(&tmp, DynamicImage::ImageRgba8(base))
            } else {
                write_png(&tmp, DynamicImage::ImageRgba8(base).into_rgb8().into())
            }
        }
    };
    if let Err(e) = written.and_then(|_| std::fs::rename(&tmp, out).map_err(Error::Io)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(PhotoOutcome {
        format,
        size,
        orientation,
        placement,
        warnings,
    })
}

fn decode(path: &Path, what: &str) -> Result<DynamicImage, Error> {
    let err = |e: &dyn std::fmt::Display| Error::MediaDecode {
        what: format!("{what} {}", path.display()),
        message: e.to_string(),
    };
    image::ImageReader::open(path)?
        .with_guessed_format()
        .map_err(|e| err(&e))?
        .decode()
        .map_err(|e| err(&e))
}

/// Apply an EXIF orientation (1 to 8) so the pixels are upright.
pub fn orient(img: DynamicImage, orientation: u8) -> DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

/// Visible part of an overlay at `(x, y)` on a `w × h` base:
/// `(base_x0, base_y0, ov_x0, ov_y0, width, height)`.
fn clip(
    base: (u32, u32),
    ov: (u32, u32),
    x: i32,
    y: i32,
) -> Option<(u32, u32, u32, u32, u32, u32)> {
    let bx0 = x.max(0) as i64;
    let by0 = y.max(0) as i64;
    let ox0 = (-(x.min(0))) as i64;
    let oy0 = (-(y.min(0))) as i64;
    let w = (ov.0 as i64 - ox0).min(base.0 as i64 - bx0);
    let h = (ov.1 as i64 - oy0).min(base.1 as i64 - by0);
    (w > 0 && h > 0).then_some((
        bx0 as u32, by0 as u32, ox0 as u32, oy0 as u32, w as u32, h as u32,
    ))
}

/// Straight-alpha "over" onto an opaque base, rounded to nearest.
pub(crate) fn blend_rgb(base: &mut RgbImage, ov: &RgbaImage, x: i32, y: i32) {
    let Some((bx, by, ox, oy, w, h)) = clip(base.dimensions(), ov.dimensions(), x, y) else {
        return;
    };
    for j in 0..h {
        for i in 0..w {
            let o = ov.get_pixel(ox + i, oy + j).0;
            let a = o[3] as u32;
            if a == 0 {
                continue;
            }
            let b = base.get_pixel_mut(bx + i, by + j);
            for (dst, src) in b.0.iter_mut().zip(o) {
                *dst = ((src as u32 * a + *dst as u32 * (255 - a) + 127) / 255) as u8;
            }
        }
    }
}

/// Porter-Duff "over" with straight alpha on both sides.
pub(crate) fn blend_rgba(base: &mut RgbaImage, ov: &RgbaImage, x: i32, y: i32) {
    let Some((bx, by, ox, oy, w, h)) = clip(base.dimensions(), ov.dimensions(), x, y) else {
        return;
    };
    for j in 0..h {
        for i in 0..w {
            let o = ov.get_pixel(ox + i, oy + j).0;
            if o[3] == 0 {
                continue;
            }
            let b = base.get_pixel_mut(bx + i, by + j);
            let ao = o[3] as f32 / 255.0;
            let ab = b.0[3] as f32 / 255.0;
            let a = ao + ab * (1.0 - ao);
            for (dst, src) in b.0.iter_mut().zip(o).take(3) {
                let v = (src as f32 * ao + *dst as f32 * ab * (1.0 - ao)) / a;
                *dst = v.round().clamp(0.0, 255.0) as u8;
            }
            b.0[3] = (a * 255.0).round() as u8;
        }
    }
}

fn write_jpeg(path: &Path, img: &RgbImage, quality: u8) -> Result<(), Error> {
    let mut w = BufWriter::new(File::create(path)?);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut w, quality.clamp(1, 100))
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
    w.flush()?;
    Ok(())
}

fn write_png(path: &Path, img: DynamicImage) -> Result<(), Error> {
    let mut w = BufWriter::new(File::create(path)?);
    img.write_to(&mut w, image::ImageFormat::Png)
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
    w.flush()?;
    Ok(())
}

/// `<dir>/.<name>.mr-partial` next to `out`.
pub(crate) fn partial_path(out: &Path) -> std::path::PathBuf {
    let name = out
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    out.with_file_name(format!(".{name}.mr-partial"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, Rgba};

    #[test]
    fn orientation_6_and_8_turn_the_photo_upright() {
        // Stored 3 wide, 2 high, red at stored top-left.
        let mut stored = RgbImage::from_pixel(3, 2, Rgb([0, 0, 0]));
        stored.put_pixel(0, 0, Rgb([255, 0, 0]));
        let up6 = orient(DynamicImage::ImageRgb8(stored.clone()), 6).into_rgb8();
        assert_eq!(up6.dimensions(), (2, 3));
        // Rotating 90 degrees clockwise moves the top-left to the top-right.
        assert_eq!(up6.get_pixel(1, 0), &Rgb([255, 0, 0]));
        let up8 = orient(DynamicImage::ImageRgb8(stored), 8).into_rgb8();
        assert_eq!(up8.dimensions(), (2, 3));
        // 90 degrees counter-clockwise moves it to the bottom-left.
        assert_eq!(up8.get_pixel(0, 2), &Rgb([255, 0, 0]));
    }

    #[test]
    fn blend_is_straight_alpha_over() {
        let mut base = RgbImage::from_pixel(4, 4, Rgb([0, 0, 200]));
        let mut ov = RgbaImage::from_pixel(2, 2, Rgba([255, 255, 255, 0]));
        ov.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        ov.put_pixel(1, 0, Rgba([255, 255, 255, 128]));
        blend_rgb(&mut base, &ov, 1, 1);
        assert_eq!(base.get_pixel(1, 1), &Rgb([255, 0, 0]));
        assert_eq!(base.get_pixel(2, 1), &Rgb([128, 128, 228]));
        assert_eq!(base.get_pixel(1, 2), &Rgb([0, 0, 200]));
        assert_eq!(base.get_pixel(0, 0), &Rgb([0, 0, 200]));
    }

    #[test]
    fn negative_offsets_crop_the_overlay() {
        let mut base = RgbImage::from_pixel(4, 2, Rgb([0, 0, 0]));
        let ov = RgbaImage::from_fn(4, 4, |_, y| Rgba([y as u8 * 60, 0, 0, 255]));
        blend_rgb(&mut base, &ov, 0, -1);
        // Overlay row 1 lands on base row 0.
        assert_eq!(base.get_pixel(0, 0), &Rgb([60, 0, 0]));
        assert_eq!(base.get_pixel(3, 1), &Rgb([120, 0, 0]));
        assert!(clip((4, 2), (4, 4), 5, 0).is_none());
    }

    #[test]
    fn rgba_over_keeps_coverage() {
        let mut base = RgbaImage::from_pixel(1, 1, Rgba([0, 0, 255, 0]));
        let ov = RgbaImage::from_pixel(1, 1, Rgba([255, 0, 0, 128]));
        blend_rgba(&mut base, &ov, 0, 0);
        assert_eq!(base.get_pixel(0, 0), &Rgba([255, 0, 0, 128]));
    }

    /// A 1x100000 overlay on a photo used to ask `resize` for hundreds of
    /// gigabytes, and a failed allocation aborts the process. Now it is a
    /// `MEDIA_DECODE` for that one item.
    #[test]
    fn absurd_overlay_is_refused_before_resizing() {
        let dir = std::env::temp_dir().join(format!("mr-composite-absurd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("main.jpg");
        let overlay = dir.join("overlay.png");
        let out = dir.join("out.jpg");
        RgbImage::from_pixel(64, 48, Rgb([10, 20, 30]))
            .save(&main)
            .unwrap();
        RgbaImage::from_pixel(1, 100_000, Rgba([255, 255, 255, 255]))
            .save(&overlay)
            .unwrap();
        let e = composite_photo(&main, &overlay, &out, &PhotoOptions::default()).unwrap_err();
        assert_eq!(e.code(), "MEDIA_DECODE", "{e}");
        assert!(!out.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
