//! Bounded pixels shared by the native image protocols and the terminal cell renderer.
use image::{
    ColorType, ImageEncoder, RgbaImage,
    codecs::png::PngEncoder,
    imageops::{FilterType, resize},
};

// After base64 and the OSC header, this is still below iTerm2's 1 MiB limit.
const MAX_PNG_BYTES: usize = 720 * 1024;

#[derive(Debug)]
pub struct Raster {
    pub dimensions: (u32, u32),
    /// Premultiplied RGB keeps transparent pixels from tinting resampled edges.
    pub pixels: RgbaImage,
    pub payload: Option<String>,
}

impl Raster {
    pub fn new(mut pixels: RgbaImage, graphics: bool, cancelled: &dyn Fn() -> bool) -> anyhow::Result<Self> {
        for row in pixels.rows_mut() {
            anyhow::ensure!(!cancelled(), "cancelled");
            for pixel in row {
                let alpha = u16::from(pixel[3]);
                if alpha != 255 {
                    for channel in &mut pixel.0[..3] {
                        *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
                    }
                }
            }
        }
        let payload = if graphics && !cancelled() { encode(&pixels, cancelled) } else { None };
        anyhow::ensure!(!cancelled(), "cancelled");
        let (w, h) = pixels.dimensions();
        let (cols, rows) = fit(w, h, 512, 256);
        if cols < w || rows < h {
            pixels = resize(&pixels, cols, rows, FilterType::Triangle);
        }
        Ok(Self { dimensions: (w, h), pixels, payload })
    }

    pub fn weight(&self) -> usize {
        self.pixels.as_raw().len() + self.payload.as_ref().map_or(0, String::capacity)
    }
}

fn encode(source: &RgbaImage, cancelled: &dyn Fn() -> bool) -> Option<String> {
    let (w, h) = source.dimensions();
    let area_scale = (524_288.0 / (f64::from(w) * f64::from(h))).sqrt();
    let scale = area_scale.min(1024.0 / f64::from(w.max(h))).min(1.0);
    let mut width = ((f64::from(w) * scale) as u32).max(1);
    let mut height = ((f64::from(h) * scale) as u32).max(1);
    for _ in 0..4 {
        if cancelled() {
            return None;
        }
        let mut image = if (width, height) == (w, h) {
            source.clone()
        } else {
            resize(source, width, height, FilterType::Lanczos3)
        };
        for pixel in image.pixels_mut() {
            let alpha = u16::from(pixel[3]);
            if (1..255).contains(&alpha) {
                for channel in &mut pixel.0[..3] {
                    *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
        let mut png = Vec::new();
        PngEncoder::new(&mut png).write_image(image.as_raw(), width, height, ColorType::Rgba8.into()).ok()?;
        if png.len() <= MAX_PNG_BYTES {
            return Some(crate::remote::base64(&png));
        }
        width = (width * 3 / 4).max(1);
        height = (height * 3 / 4).max(1);
    }
    None
}

/// Fit a source aspect ratio inside a pixel rectangle.
pub fn fit(iw: u32, ih: u32, w: u32, h: u32) -> (u32, u32) {
    let (iw, ih) = (u64::from(iw), u64::from(ih));
    if iw * u64::from(h) > ih * u64::from(w) {
        (w, (ih * u64::from(w)).div_ceil(iw).max(1) as u32)
    } else {
        ((iw * u64::from(h)).div_ceil(ih).max(1) as u32, h)
    }
}

pub fn fit_cells((iw, ih): (u32, u32), (cw, ch): (u32, u32), (w, h): (u16, u16), (sx, sy): (u32, u32)) -> (u32, u32) {
    fit(iw * ch * sx, ih * cw * sy, u32::from(w) * sx, u32::from(h) * sy)
}
