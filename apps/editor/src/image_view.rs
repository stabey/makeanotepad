use std::sync::Arc;

use editor_services::{
    CancelToken, DocMeta, DocumentId, LifecycleGen, RasterImageFormat, ViewState,
};

/// Workspace-wide budget for decoded BGRA `RenderImage` buffers retained across open image tabs
/// (256 MiB total across hydrated image previews).
pub const MAX_WORKSPACE_IMAGE_BYTES: u64 = 256 * 1024 * 1024;

pub const MIN_IMAGE_ZOOM_PCT: u16 = 10;
pub const MAX_IMAGE_ZOOM_PCT: u16 = 800;

/// Ordered zoom preset steps (percentage) used by Zoom In / Zoom Out navigation.
pub const IMAGE_ZOOM_STEPS: [u16; 13] =
    [10, 25, 50, 67, 75, 100, 125, 150, 200, 300, 400, 600, 800];

/// Zoom mode for a raster image viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageZoomMode {
    Fit,
    Percent(u16),
}

impl ImageZoomMode {
    /// Steps to the next higher zoom preset. When in `Fit` mode, `fit_pct` supplies the current
    /// effective fit percentage (defaulting to `100` when layout dimensions are not yet known).
    pub fn zoom_in(self, fit_pct: Option<u16>) -> Self {
        let current = match self {
            Self::Fit => fit_pct.unwrap_or(100),
            Self::Percent(pct) => pct,
        };
        let next = IMAGE_ZOOM_STEPS
            .iter()
            .copied()
            .find(|&step| step > current)
            .unwrap_or(MAX_IMAGE_ZOOM_PCT);
        Self::Percent(next)
    }

    /// Steps to the next lower zoom preset. When in `Fit` mode, `fit_pct` supplies the current
    /// effective fit percentage (defaulting to `100` when layout dimensions are not yet known).
    pub fn zoom_out(self, fit_pct: Option<u16>) -> Self {
        let current = match self {
            Self::Fit => fit_pct.unwrap_or(100),
            Self::Percent(pct) => pct,
        };
        let prev = IMAGE_ZOOM_STEPS
            .iter()
            .rev()
            .copied()
            .find(|&step| step < current)
            .unwrap_or(MIN_IMAGE_ZOOM_PCT);
        Self::Percent(prev)
    }

    /// Restores zoom mode from persisted `ViewState` (`0` = `Fit`, `10..=800` = `Percent(p)`).
    pub fn from_view_state(view: &ViewState) -> Self {
        let raw = if view.cursor_byte != 0 {
            view.cursor_byte
        } else {
            view.scroll_byte
        };
        if raw == 0 {
            Self::Fit
        } else if (MIN_IMAGE_ZOOM_PCT as u64..=MAX_IMAGE_ZOOM_PCT as u64).contains(&raw) {
            Self::Percent(raw as u16)
        } else {
            Self::Fit
        }
    }

    /// Encodes zoom mode into `ViewState` (`0` = `Fit`, `10..=800` = `Percent(p)`).
    pub fn to_view_state(self) -> ViewState {
        let zoom = match self {
            Self::Fit => 0,
            Self::Percent(pct) => pct.clamp(MIN_IMAGE_ZOOM_PCT, MAX_IMAGE_ZOOM_PCT) as u64,
        };
        ViewState {
            cursor_byte: zoom,
            scroll_byte: 0,
        }
    }

    /// Short display label for the image toolbar / status bar.
    pub fn label(self) -> String {
        match self {
            Self::Fit => "Fit".to_string(),
            Self::Percent(pct) => format!("{pct}%"),
        }
    }

    /// Computes `(display_width, display_height, effective_zoom_pct)` for an image of
    /// `img_w x img_h` inside a viewport of `avail_w x avail_h`.
    ///
    /// `Fit` scales down proportionally to fit inside `avail_w x avail_h` without upscaling images
    /// that already fit at `100%`.
    pub fn compute_display_size(
        self,
        img_w: u32,
        img_h: u32,
        avail_w: f32,
        avail_h: f32,
    ) -> (f32, f32, u16) {
        if img_w == 0 || img_h == 0 {
            let pct = match self {
                Self::Fit => 100,
                Self::Percent(p) => p.clamp(MIN_IMAGE_ZOOM_PCT, MAX_IMAGE_ZOOM_PCT),
            };
            return (0.0, 0.0, pct);
        }

        let iw = img_w as f32;
        let ih = img_h as f32;
        match self {
            Self::Percent(pct) => {
                let pct = pct.clamp(MIN_IMAGE_ZOOM_PCT, MAX_IMAGE_ZOOM_PCT);
                let scale = pct as f32 / 100.0;
                (iw * scale, ih * scale, pct)
            }
            Self::Fit => {
                if !avail_w.is_finite() || !avail_h.is_finite() || avail_w <= 0.0 || avail_h <= 0.0
                {
                    return (iw, ih, 100);
                }
                let scale = (avail_w / iw).min(avail_h / ih).min(1.0);
                let pct = (scale * 100.0).round().clamp(1.0, 100.0) as u16;
                (iw * scale, ih * scale, pct)
            }
        }
    }
}

/// Per-pane/per-view viewport state for an open raster image preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageViewportState {
    pub zoom: ImageZoomMode,
}

impl Default for ImageViewportState {
    fn default() -> Self {
        Self {
            zoom: ImageZoomMode::Fit,
        }
    }
}

impl ImageViewportState {
    pub fn new_from_view(view: &ViewState) -> Self {
        Self {
            zoom: ImageZoomMode::from_view_state(view),
        }
    }

    pub fn capture_view_state(&self) -> ViewState {
        self.zoom.to_view_state()
    }
}

/// Decoded GPU-uploadable image payload lifecycle for a shared `ImageTabState`.
#[derive(Clone, Debug)]
pub enum ImagePayloadState {
    Decoding {
        req_seq: u64,
        cancel: CancelToken,
    },
    Ready {
        render_image: Arc<gpui_kit::RenderImage>,
        decoded_bytes: u64,
    },
    Dehydrated,
    OverBudget {
        reason: String,
    },
    Failed {
        error: String,
    },
}

/// Per-document state for a read-only raster image preview (`T15`).
pub struct ImageTabState {
    pub doc_id: DocumentId,
    pub lifecycle_gen: LifecycleGen,
    pub meta: DocMeta,
    pub format: RasterImageFormat,
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    pub file_bytes: u64,
    pub estimated_decoded_bytes: u64,
    pub last_activated_tick: u64,
    pub state: ImagePayloadState,
}

impl Drop for ImageTabState {
    fn drop(&mut self) {
        if let ImagePayloadState::Decoding { cancel, .. } = &self.state {
            cancel.cancel();
        }
    }
}

impl ImageTabState {
    pub fn format_label(&self) -> &'static str {
        match self.format {
            RasterImageFormat::Png => "PNG",
            RasterImageFormat::Jpeg => "JPEG",
            RasterImageFormat::Webp => "WebP",
            RasterImageFormat::Gif => "GIF",
            RasterImageFormat::Bmp => "BMP",
            RasterImageFormat::Ico => "ICO",
            RasterImageFormat::Tiff => "TIFF",
        }
    }

    pub fn gpui_format(&self) -> gpui_kit::ImageFormat {
        match self.format {
            RasterImageFormat::Png => gpui_kit::ImageFormat::Png,
            RasterImageFormat::Jpeg => gpui_kit::ImageFormat::Jpeg,
            RasterImageFormat::Webp => gpui_kit::ImageFormat::Webp,
            RasterImageFormat::Gif => gpui_kit::ImageFormat::Gif,
            RasterImageFormat::Bmp => gpui_kit::ImageFormat::Bmp,
            RasterImageFormat::Ico => gpui_kit::ImageFormat::Ico,
            RasterImageFormat::Tiff => gpui_kit::ImageFormat::Tiff,
        }
    }

    /// Returns the decoded RGBA/BGRA byte size held when `Ready`, or `0` otherwise.
    pub fn ready_decoded_bytes(&self) -> u64 {
        match &self.state {
            ImagePayloadState::Ready { decoded_bytes, .. } => *decoded_bytes,
            _ => 0,
        }
    }

    /// Transitions a `Ready` image to `Dehydrated` and drops its sprite-atlas texture from GPUI.
    pub fn release_image_from_gpui(
        &mut self,
        window: Option<&mut gpui_kit::Window>,
        cx: &mut gpui_kit::App,
    ) -> bool {
        if !matches!(self.state, ImagePayloadState::Ready { .. }) {
            return false;
        }
        let prev = std::mem::replace(&mut self.state, ImagePayloadState::Dehydrated);
        if let ImagePayloadState::Ready { render_image, .. } = prev {
            cx.drop_image(render_image, window);
            return true;
        }
        false
    }

    /// Plans LRU eviction of non-visible `Ready` images so `sum + incoming_bytes <= max_bytes`.
    ///
    /// Each entry in `slots` is `(doc_id, decoded_bytes, is_visible, last_activated_tick)`.
    /// Returns `None` if visible/active images plus `incoming_bytes` alone exceed `max_bytes`.
    pub fn plan_image_cache_eviction(
        slots: &[(DocumentId, u64, bool, u64)],
        incoming_bytes: u64,
        max_bytes: u64,
    ) -> Option<Vec<DocumentId>> {
        plan_image_cache_eviction(slots, incoming_bytes, max_bytes)
    }
}

/// Plans LRU eviction of non-visible `Ready` images so `sum + incoming_bytes <= max_bytes`.
///
/// Each entry in `slots` is `(doc_id, decoded_bytes, is_visible, last_activated_tick)`.
/// Returns `None` if visible/active images plus `incoming_bytes` alone exceed `max_bytes`.
pub fn plan_image_cache_eviction(
    slots: &[(DocumentId, u64, bool, u64)],
    incoming_bytes: u64,
    max_bytes: u64,
) -> Option<Vec<DocumentId>> {
    let mut remaining_bytes: u128 = slots.iter().map(|&(_, bytes, _, _)| bytes as u128).sum();
    let incoming = incoming_bytes as u128;
    let cap = max_bytes as u128;

    if remaining_bytes + incoming <= cap {
        return Some(Vec::new());
    }

    let mut evictable: Vec<(DocumentId, u64, u64)> = slots
        .iter()
        .copied()
        .filter(|&(_, bytes, is_visible, _)| !is_visible && bytes > 0)
        .map(|(doc_id, bytes, _, tick)| (doc_id, bytes, tick))
        .collect();
    evictable.sort_by_key(|&(_, _, tick)| tick);

    let mut evicted = Vec::new();
    for (doc_id, bytes, _) in evictable {
        if remaining_bytes + incoming <= cap {
            break;
        }
        evicted.push(doc_id);
        remaining_bytes = remaining_bytes.saturating_sub(bytes as u128);
    }

    if remaining_bytes + incoming <= cap {
        Some(evicted)
    } else {
        None
    }
}

/// Sums the raw decoded BGRA buffer bytes across all frames of a `RenderImage`.
pub fn render_image_decoded_bytes(render_image: &gpui_kit::RenderImage) -> u64 {
    (0..render_image.frame_count())
        .map(|frame_idx| {
            render_image
                .as_bytes(frame_idx)
                .map_or(0, |b| b.len() as u64)
        })
        .sum()
}

/// Decodes raw raster bytes into a GPUI `RenderImage` on a background thread.
///
/// `gpui_kit::SvgRenderer` is `Send + Sync` (all fields are `Arc<dyn AssetSource>` where
/// `AssetSource: Send + Sync` and `Arc<usvg::Options<'static>>` whose resolver callbacks require
/// `Send + Sync`), so passing a cloned `SvgRenderer` off-thread is sound and matches GPUI's own
/// `ImageDecoder` asset loader.
pub fn decode_render_image_off_thread(
    format: gpui_kit::ImageFormat,
    bytes: Arc<[u8]>,
    svg_renderer: gpui_kit::SvgRenderer,
    cancel: &CancelToken,
) -> Result<Arc<gpui_kit::RenderImage>, String> {
    if cancel.is_cancelled() {
        return Err("image decode cancelled".to_string());
    }
    let image = gpui_kit::Image::from_bytes(format, bytes.to_vec());
    let render_image = image
        .to_image_data(svg_renderer)
        .map_err(|err| format!("failed to decode image: {err}"))?;
    if cancel.is_cancelled() {
        return Err("image decode cancelled".to_string());
    }
    Ok(render_image)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_1X1_RED: [u8; 70] = [
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    #[test]
    fn zoom_steps_advance_and_clamp_properly() {
        let mut mode = ImageZoomMode::Percent(10);
        assert_eq!(mode.zoom_out(None), ImageZoomMode::Percent(10));

        let mut visited = vec![10];
        for _ in 0..20 {
            mode = mode.zoom_in(None);
            if let ImageZoomMode::Percent(pct) = mode {
                if visited.last().copied() != Some(pct) {
                    visited.push(pct);
                }
            }
        }
        assert_eq!(visited, IMAGE_ZOOM_STEPS);
        assert_eq!(
            ImageZoomMode::Percent(800).zoom_in(None),
            ImageZoomMode::Percent(800)
        );

        assert_eq!(
            ImageZoomMode::Percent(90).zoom_in(None),
            ImageZoomMode::Percent(100)
        );
        assert_eq!(
            ImageZoomMode::Percent(90).zoom_out(None),
            ImageZoomMode::Percent(75)
        );

        assert_eq!(
            ImageZoomMode::Fit.zoom_in(Some(33)),
            ImageZoomMode::Percent(50)
        );
        assert_eq!(
            ImageZoomMode::Fit.zoom_out(Some(33)),
            ImageZoomMode::Percent(25)
        );
        assert_eq!(
            ImageZoomMode::Fit.zoom_in(None),
            ImageZoomMode::Percent(125)
        );
        assert_eq!(
            ImageZoomMode::Fit.zoom_out(None),
            ImageZoomMode::Percent(75)
        );
        assert_eq!(ImageZoomMode::Fit.label(), "Fit");
        assert_eq!(ImageZoomMode::Percent(125).label(), "125%");
    }

    #[test]
    fn fit_display_scaling_downscales_proportionally_without_upscaling() {
        let (w, h, pct) = ImageZoomMode::Fit.compute_display_size(2000, 1000, 1000.0, 800.0);
        assert!((w - 1000.0).abs() < 1e-3);
        assert!((h - 500.0).abs() < 1e-3);
        assert_eq!(pct, 50);

        let (w, h, pct) = ImageZoomMode::Fit.compute_display_size(1000, 2000, 1000.0, 500.0);
        assert!((w - 250.0).abs() < 1e-3);
        assert!((h - 500.0).abs() < 1e-3);
        assert_eq!(pct, 25);

        // Smaller image must never be upscaled in Fit mode.
        let (w, h, pct) = ImageZoomMode::Fit.compute_display_size(320, 240, 1280.0, 800.0);
        assert_eq!((w, h, pct), (320.0, 240.0, 100));

        // Explicit percentage scales proportionally regardless of viewport bounds.
        let (w, h, pct) = ImageZoomMode::Percent(200).compute_display_size(320, 240, 100.0, 100.0);
        assert_eq!((w, h, pct), (640.0, 480.0, 200));

        // Zero dimensions or non-positive viewport bounds never produce NaN.
        assert_eq!(
            ImageZoomMode::Fit.compute_display_size(0, 100, 800.0, 600.0),
            (0.0, 0.0, 100)
        );
        assert_eq!(
            ImageZoomMode::Fit.compute_display_size(400, 300, 0.0, 600.0),
            (400.0, 300.0, 100)
        );
    }

    #[test]
    fn view_state_round_trips_zoom_modes() {
        let fit_view = ImageViewportState::default().capture_view_state();
        assert_eq!(fit_view, ViewState::default());
        assert_eq!(
            ImageViewportState::new_from_view(&fit_view).zoom,
            ImageZoomMode::Fit
        );

        for &pct in &IMAGE_ZOOM_STEPS {
            let vp = ImageViewportState {
                zoom: ImageZoomMode::Percent(pct),
            };
            let captured = vp.capture_view_state();
            let restored = ImageViewportState::new_from_view(&captured);
            assert_eq!(restored.zoom, ImageZoomMode::Percent(pct));
        }

        // Out-of-range legacy byte offsets fall back safely to Fit.
        assert_eq!(
            ImageZoomMode::from_view_state(&ViewState {
                cursor_byte: 5,
                scroll_byte: 0,
            }),
            ImageZoomMode::Fit
        );
        assert_eq!(
            ImageZoomMode::from_view_state(&ViewState {
                cursor_byte: 5_000,
                scroll_byte: 0,
            }),
            ImageZoomMode::Fit
        );
    }

    #[test]
    fn image_cache_eviction_plans_lru_non_visible_and_protects_visible_budget() {
        let d1 = DocumentId(1);
        let d2 = DocumentId(2);
        let d3 = DocumentId(3);
        let d4 = DocumentId(4);

        // Total ready = 240, cap = 250, incoming = 100 -> need to free >= 90.
        // d1 is visible (protected), d2 (tick 5) and d4 (tick 10) are non-visible, d3 (tick 20) is MRU non-visible.
        let slots = vec![
            (d1, 60, true, 1),
            (d2, 50, false, 5),
            (d3, 60, false, 20),
            (d4, 70, false, 10),
        ];
        let plan = plan_image_cache_eviction(&slots, 100, 250).expect("should fit after eviction");
        // Evicting d2 (50) leaves 190 + 100 = 290 > 250; evicting d4 (70) leaves 120 + 100 = 220 <= 250.
        assert_eq!(plan, vec![d2, d4]);

        // Under budget needs no eviction.
        assert_eq!(plan_image_cache_eviction(&slots, 10, 250), Some(Vec::new()));

        // Active/visible images + incoming exceeding max_bytes returns None.
        let visible_heavy = vec![(d1, 200, true, 1), (d2, 50, false, 2)];
        assert_eq!(plan_image_cache_eviction(&visible_heavy, 60, 250), None);
    }

    #[test]
    fn svg_renderer_is_send_sync_and_off_thread_decode_works_and_respects_cancel() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<gpui_kit::SvgRenderer>();

        let renderer = gpui_kit::SvgRenderer::new(Arc::new(()));
        let bytes: Arc<[u8]> = Arc::from(PNG_1X1_RED.as_slice());

        let cancelled = CancelToken::new();
        cancelled.cancel();
        let err = decode_render_image_off_thread(
            gpui_kit::ImageFormat::Png,
            bytes.clone(),
            renderer.clone(),
            &cancelled,
        )
        .expect_err("cancelled decode should fail early");
        assert!(err.contains("cancelled"));

        let active = CancelToken::new();
        let active_clone = active.clone();
        let render_image = std::thread::spawn(move || {
            decode_render_image_off_thread(
                gpui_kit::ImageFormat::Png,
                bytes,
                renderer,
                &active_clone,
            )
        })
        .join()
        .expect("decode thread should not panic")
        .expect("1x1 PNG should decode off-thread");

        assert_eq!(render_image.frame_count(), 1);
        assert_eq!(
            render_image.size(0),
            gpui_kit::size(gpui_kit::DevicePixels(1), gpui_kit::DevicePixels(1))
        );
        assert_eq!(render_image_decoded_bytes(&render_image), 4);
    }

    #[test]
    fn image_tab_state_maps_formats_and_reports_ready_bytes() {
        let tab = ImageTabState {
            doc_id: DocumentId(42),
            lifecycle_gen: LifecycleGen(1),
            meta: DocMeta::default(),
            format: RasterImageFormat::Webp,
            width: 16,
            height: 8,
            frames: 1,
            file_bytes: 128,
            estimated_decoded_bytes: 16 * 8 * 4,
            last_activated_tick: 3,
            state: ImagePayloadState::Dehydrated,
        };
        assert_eq!(tab.format_label(), "WebP");
        assert_eq!(tab.gpui_format(), gpui_kit::ImageFormat::Webp);
        assert_eq!(tab.ready_decoded_bytes(), 0);
    }
}
