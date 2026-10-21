use anyhow::{Result, anyhow, bail};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PopupBounds {
    x: i64,
    y: i64,
    width: i32,
    height: i32,
}

#[derive(Default)]
pub(crate) struct BrowserFrames {
    view: Option<Pixels>,
    popup: Option<Pixels>,
    popup_bounds: Option<PopupBounds>,
    popup_visible: bool,
}

impl BrowserFrames {
    pub(crate) fn paint_view(&mut self, width: i32, height: i32, bytes: &[u8]) -> Result<()> {
        let view = checked_pixels(width, height, bytes)?;
        if self
            .view
            .as_ref()
            .is_some_and(|previous| (previous.width, previous.height) != (view.width, view.height))
        {
            self.clear_popup();
        }
        self.view = Some(view);
        Ok(())
    }

    pub(crate) fn paint_popup(&mut self, width: i32, height: i32, bytes: &[u8]) -> Result<()> {
        let popup = checked_pixels(width, height, bytes)?;
        if self.popup_visible && self.popup_bounds.is_some() {
            self.popup = Some(popup);
        }
        Ok(())
    }

    pub(crate) fn show_popup(&mut self, visible: bool) {
        self.popup_visible = visible;
        if !visible {
            self.popup = None;
            self.popup_bounds = None;
        }
    }

    pub(crate) fn size_popup(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        scale: f32,
    ) -> Result<()> {
        if width <= 0 || height <= 0 || !scale.is_finite() || scale <= 0. {
            bail!("Invalid browser popup bounds or scale");
        }
        frame_bytes(
            (f64::from(width) * f64::from(scale)).ceil() as i32,
            (f64::from(height) * f64::from(scale)).ceil() as i32,
        )?;
        let bounds = PopupBounds {
            x: (f64::from(x) * f64::from(scale)).round() as i64,
            y: (f64::from(y) * f64::from(scale)).round() as i64,
            width,
            height,
        };
        if self
            .popup_bounds
            .is_some_and(|previous| (previous.width, previous.height) != (width, height))
        {
            self.popup = None;
        }
        self.popup_bounds = Some(bounds);
        Ok(())
    }

    pub(crate) fn clear_popup(&mut self) {
        self.popup = None;
        self.popup_bounds = None;
        self.popup_visible = false;
    }

    pub(crate) fn compose(&self) -> Option<Pixels> {
        let mut output = self.view.clone()?;
        if self.popup_visible {
            if let (Some(popup), Some(bounds)) = (&self.popup, self.popup_bounds) {
                composite_popup(&mut output, popup, bounds);
            }
        }
        unpremultiply(&mut output.bgra);
        Some(output)
    }
}

fn composite_popup(view: &mut Pixels, popup: &Pixels, bounds: PopupBounds) {
    let left = bounds.x.max(0).min(i64::from(view.width));
    let top = bounds.y.max(0).min(i64::from(view.height));
    let right = bounds
        .x
        .saturating_add(i64::from(popup.width))
        .max(0)
        .min(i64::from(view.width));
    let bottom = bounds
        .y
        .saturating_add(i64::from(popup.height))
        .max(0)
        .min(i64::from(view.height));
    if left >= right || top >= bottom {
        return;
    }
    let view_rows = view
        .bgra
        .chunks_exact_mut(view.width as usize * 4)
        .skip(top as usize);
    let popup_rows = popup
        .bgra
        .chunks_exact(popup.width as usize * 4)
        .skip((top - bounds.y) as usize);
    for (view_row, popup_row) in view_rows.zip(popup_rows).take((bottom - top) as usize) {
        let view_pixels = view_row
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .skip(left as usize);
        let popup_pixels = popup_row
            .as_chunks::<4>()
            .0
            .iter()
            .skip((left - bounds.x) as usize);
        for (destination, source) in view_pixels.zip(popup_pixels).take((right - left) as usize) {
            let inverse_alpha = 255 - u32::from(source[3]);
            // CEF supplies premultiplied BGRA, so blend before GPUI's straight-alpha conversion.
            for (destination, source) in destination.iter_mut().zip(source) {
                *destination = (u32::from(*source)
                    + (u32::from(*destination) * inverse_alpha + 127) / 255)
                    .min(255) as u8;
            }
        }
    }
}

fn checked_pixels(width: i32, height: i32, bytes: &[u8]) -> Result<Pixels> {
    if bytes.len() != frame_bytes(width, height)? {
        bail!("Invalid browser frame length");
    }
    Ok(Pixels {
        width: width as u32,
        height: height as u32,
        bgra: bytes.to_vec(),
    })
}

fn unpremultiply(bytes: &mut [u8]) {
    for pixel in bytes.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        if alpha == 255 {
            continue;
        }
        if alpha == 0 {
            pixel.fill(0);
            continue;
        }
        for channel in pixel.iter_mut().take(3) {
            *channel = (u32::from(*channel) * 255 + alpha / 2)
                .checked_div(alpha)
                .unwrap_or(0)
                .min(255) as u8;
        }
    }
}

pub(crate) fn raster_scale(width: i32, height: i32, requested: f32) -> Result<f32> {
    if width <= 0 || height <= 0 || !requested.is_finite() || requested <= 0. {
        bail!("Invalid browser viewport or scale");
    }
    let fits = |scale: f32, rounding_margin: i32| {
        let dimension = |length: i32| {
            let double = (f64::from(length) * f64::from(scale)).ceil();
            let single = (length as f32 * scale).ceil();
            (double.max(f64::from(single)).max(1.) as i32).saturating_add(rounding_margin)
        };
        frame_bytes(dimension(width), dimension(height)).is_ok()
    };
    if fits(requested, 0) {
        return Ok(requested);
    }
    // Positive finite f32 bit patterns are ordered; keep logical geometry while
    // selecting the largest raster scale that leaves room for CEF rounding.
    let mut lower = 0;
    let mut upper = requested.to_bits();
    while lower < upper {
        let middle = lower + (upper - lower).div_ceil(2);
        if fits(f32::from_bits(middle), 1) {
            lower = middle;
        } else {
            upper = middle - 1;
        }
    }
    let scale = f32::from_bits(lower);
    if scale <= 0. {
        bail!("Browser viewport cannot fit the bounded raster budget");
    }
    Ok(scale)
}

pub(crate) fn frame_bytes(width: i32, height: i32) -> Result<usize> {
    if width <= 0 || height <= 0 {
        bail!("Invalid browser frame dimensions");
    }
    let length = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| anyhow!("Browser frame size overflow"))?;
    if length > 16 * 1024 * 1024 {
        bail!("Browser frame exceeds 16 MiB limit");
    }
    Ok(length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_conversion_matches_reference_for_every_channel_and_alpha() {
        for alpha in 0..=255u8 {
            for channel in 0..=255u8 {
                let original = [channel, 255 - channel, channel / 2, alpha];
                let mut expected = original;
                for channel in expected.iter_mut().take(3) {
                    *channel = (u32::from(*channel) * 255 + u32::from(alpha) / 2)
                        .checked_div(u32::from(alpha))
                        .unwrap_or(0)
                        .min(255) as u8;
                }
                let mut actual = original;
                unpremultiply(&mut actual);
                assert_eq!(actual, expected);
            }
        }
    }

    #[test]
    fn frame_validation_precedes_copy_and_preserves_alpha() -> Result<()> {
        assert!(frame_bytes(0, 1).is_err());
        assert!(frame_bytes(i32::MAX, i32::MAX).is_err());
        assert!(checked_pixels(1, 1, &[0, 0, 0]).is_err());
        assert_eq!(
            {
                let mut bytes = vec![16, 32, 64, 128];
                unpremultiply(&mut bytes);
                bytes
            },
            vec![32, 64, 128, 128]
        );
        Ok(())
    }

    #[test]
    fn large_high_dpi_viewports_keep_raster_frames_bounded() -> Result<()> {
        for (width, height, requested) in [
            (3840, 2160, 2.),
            (2560, 1440, 2.),
            (7680, 4320, 1.),
            (i32::MAX, 1, f32::MAX),
            (i32::MAX, i32::MAX, f32::MAX),
        ] {
            let scale = raster_scale(width, height, requested)?;
            assert!(scale > 0. && scale <= requested);
            assert!(
                frame_bytes(
                    (width as f64 * f64::from(scale)).ceil() as i32,
                    (height as f64 * f64::from(scale)).ceil() as i32
                )
                .is_ok()
            );
        }
        assert_eq!(raster_scale(1200, 800, 1.)?, 1.);
        assert_eq!(raster_scale(1280, 720, 1.5)?, 1.5);
        for scale in [0., -1., f32::NAN, f32::INFINITY] {
            assert!(raster_scale(1200, 800, scale).is_err());
        }
        assert!(raster_scale(0, 800, 1.).is_err());
        Ok(())
    }

    fn solid(width: i32, height: i32, pixel: [u8; 4]) -> Vec<u8> {
        pixel.repeat((width * height) as usize)
    }

    fn open_popup(
        frames: &mut BrowserFrames,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        scale: f32,
        pixel: [u8; 4],
    ) -> Result<()> {
        frames.show_popup(true);
        frames.size_popup(x, y, width, height, scale)?;
        frames.paint_popup(
            (width as f32 * scale).ceil() as i32,
            (height as f32 * scale).ceil() as i32,
            &solid(
                (width as f32 * scale).ceil() as i32,
                (height as f32 * scale).ceil() as i32,
                pixel,
            ),
        )
    }

    #[test]
    fn popup_paints_over_view_and_hide_restores_view() -> Result<()> {
        let mut frames = BrowserFrames::default();
        let view = solid(3, 2, [10, 20, 30, 255]);
        frames.paint_view(3, 2, &view)?;
        open_popup(&mut frames, 1, 0, 1, 1, 1., [60, 70, 80, 255])?;
        let expected = [[10, 20, 30, 255], [60, 70, 80, 255], [10, 20, 30, 255]].concat();
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            [expected, solid(3, 1, [10, 20, 30, 255])].concat()
        );
        frames.show_popup(false);
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            view
        );
        frames.paint_popup(1, 1, &[60, 70, 80, 255])?;
        frames.show_popup(true);
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            view
        );
        Ok(())
    }

    #[test]
    fn view_repaint_preserves_popup_and_move_restores_old_location() -> Result<()> {
        let mut frames = BrowserFrames::default();
        frames.paint_view(3, 1, &solid(3, 1, [0, 0, 0, 255]))?;
        open_popup(&mut frames, 0, 0, 1, 1, 1., [255, 0, 0, 255])?;
        frames.paint_view(3, 1, &solid(3, 1, [0, 255, 0, 255]))?;
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            [[255, 0, 0, 255], [0, 255, 0, 255], [0, 255, 0, 255]].concat()
        );
        frames.size_popup(2, 0, 1, 1, 1.)?;
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            [[0, 255, 0, 255], [0, 255, 0, 255], [255, 0, 0, 255]].concat()
        );
        Ok(())
    }

    #[test]
    fn popup_uses_device_scale_and_clips_all_edges() -> Result<()> {
        let mut frames = BrowserFrames::default();
        let background = [0, 0, 0, 255];
        let foreground = [255, 0, 0, 255];
        frames.paint_view(4, 4, &solid(4, 4, background))?;
        open_popup(&mut frames, 1, 1, 1, 1, 1.5, foreground)?;
        let mut expected = solid(4, 2, background);
        expected.extend(
            [background, background, foreground, foreground]
                .concat()
                .repeat(2),
        );
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            expected
        );
        open_popup(&mut frames, -1, -1, 2, 2, 1., foreground)?;
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            [
                foreground.to_vec(),
                solid(3, 1, background),
                solid(4, 3, background)
            ]
            .concat()
        );
        frames.size_popup(i32::MAX, i32::MAX, 2, 2, 1.)?;
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            solid(4, 4, background)
        );
        frames.size_popup(3, 3, 2, 2, 1.)?;
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            [
                solid(4, 3, background),
                solid(3, 1, background),
                foreground.to_vec()
            ]
            .concat()
        );
        Ok(())
    }

    #[test]
    fn popup_composites_premultiplied_alpha_before_unpremultiplying() -> Result<()> {
        let mut frames = BrowserFrames::default();
        frames.paint_view(1, 1, &[0, 64, 0, 128])?;
        open_popup(&mut frames, 0, 0, 1, 1, 1., [64, 0, 0, 128])?;
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            [85, 43, 0, 192]
        );
        frames.paint_popup(1, 1, &[0, 0, 0, 0])?;
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            [0, 128, 0, 128]
        );
        frames.paint_view(1, 1, &[0, 0, 0, 0])?;
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            [0, 0, 0, 0]
        );
        Ok(())
    }

    #[test]
    fn viewport_and_popup_resize_discard_stale_popup_pixels() -> Result<()> {
        let mut frames = BrowserFrames::default();
        frames.paint_view(2, 1, &solid(2, 1, [0, 0, 0, 255]))?;
        open_popup(&mut frames, 0, 0, 1, 1, 1., [255, 0, 0, 255])?;
        frames.size_popup(0, 0, 2, 1, 1.)?;
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            solid(2, 1, [0, 0, 0, 255])
        );
        open_popup(&mut frames, 0, 0, 1, 1, 1., [255, 0, 0, 255])?;
        frames.clear_popup();
        frames.paint_popup(1, 1, &[255, 0, 0, 255])?;
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            solid(2, 1, [0, 0, 0, 255])
        );
        open_popup(&mut frames, 0, 0, 1, 1, 1., [255, 0, 0, 255])?;
        frames.paint_view(1, 1, &[0, 255, 0, 255])?;
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            [0, 255, 0, 255]
        );
        Ok(())
    }

    #[test]
    fn malformed_frames_and_popup_bounds_are_rejected_without_replacing_view() -> Result<()> {
        let mut frames = BrowserFrames::default();
        assert!(frames.compose().is_none());
        frames.paint_view(1, 1, &[0, 0, 0, 255])?;
        for (width, height, bytes) in [
            (0, 1, &[][..]),
            (-1, 1, &[][..]),
            (i32::MAX, i32::MAX, &[][..]),
            (2049, 2048, &[][..]),
            (1, 1, &[0, 0, 0][..]),
            (1, 1, &[0; 5][..]),
        ] {
            assert!(frames.paint_view(width, height, bytes).is_err());
            assert!(frames.paint_popup(width, height, bytes).is_err());
        }
        for (width, height, scale) in [
            (0, 1, 1.),
            (-1, 1, 1.),
            (1, 1, 0.),
            (1, 1, f32::NAN),
            (1, 1, f32::INFINITY),
            (i32::MAX, i32::MAX, 1.),
        ] {
            assert!(frames.size_popup(0, 0, width, height, scale).is_err());
        }
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            [0, 0, 0, 255]
        );
        Ok(())
    }

    #[test]
    fn clipping_keeps_popup_source_rows_and_columns_aligned() -> Result<()> {
        let mut frames = BrowserFrames::default();
        frames.paint_view(2, 2, &solid(2, 2, [0, 0, 0, 255]))?;
        frames.show_popup(true);
        frames.size_popup(-1, -1, 2, 2, 1.)?;
        frames.paint_popup(
            2,
            2,
            &[
                [10, 0, 0, 255],
                [20, 0, 0, 255],
                [30, 0, 0, 255],
                [40, 0, 0, 255],
            ]
            .concat(),
        )?;
        assert_eq!(
            frames
                .compose()
                .ok_or_else(|| anyhow!("missing view"))?
                .bgra,
            [
                [40, 0, 0, 255],
                [0, 0, 0, 255],
                [0, 0, 0, 255],
                [0, 0, 0, 255]
            ]
            .concat()
        );
        Ok(())
    }
}
