use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use editor_services::{
    CancelToken, DocMeta, DocumentId, EncodingSpec, ErrorKind, IndexStatus, LifecycleGen,
    LineEnding, PagedDocHandle, ReadOnlyReason, ServiceError, SourceGeneration, VIEWPORT_BYTES,
    ViewState, ViewportAnchor, ViewportRequest, ViewportSlice,
};
use gpui_kit::component::VirtualListScrollHandle;
use gpui_kit::{Pixels, ScrollStrategy, Size, px, size};

/// Step size for PageUp / PageDown navigation inside the 64 KiB bounded viewport (48 KiB step
/// leaves 16 KiB overlap context so lines straddling page boundaries remain easy to read).
pub const PAGE_STEP_BYTES: u64 = 48 * 1024;

/// Maximum characters rendered on one visual line row in Paged View before wrapping into a
/// continuation segment (`↪`). Keeps CoreText/GPUI shaping strictly bounded even on a 128 MiB
/// single-line file (`L03`) without counting continuation wraps as logical file lines (`§6.4`).
pub const MAX_DISPLAY_SEGMENT_CHARS: usize = 180;

pub const SEGMENT_ROW_HEIGHT_PX: f32 = 20.0;
pub const SEGMENT_ROW_MEASURE_WIDTH_PX: f32 = 1600.0;

#[derive(Clone, Debug)]
pub struct DisplaySegment {
    /// 1-based logical file line number (only on the first segment of a logical line, when known).
    pub logical_line_1based: Option<u64>,
    /// Exact raw file byte range represented by this visual segment when `!slice.lossy`.
    /// `None` when `slice.lossy == true` (invalid UTF-8 bytes expand to 3-byte `\u{FFFD}`,
    /// so decoded string byte offsets do not map 1:1 to raw file byte offsets).
    pub exact_file_byte_range: Option<Range<u64>>,
    /// Whether this row is a continuation fragment of a long line (`↪`), NOT a new logical line.
    pub is_continuation: bool,
    /// Slice into `ViewportSlice::text` (`char`-boundary aligned).
    pub text_range: Range<usize>,
}

/// Pure segmenter that splits a `<= 64 KiB` `ViewportSlice` into bounded display rows without
/// allocating per-line `String`s.
pub fn build_display_segments(slice: &ViewportSlice) -> Vec<DisplaySegment> {
    let text: &str = &slice.text;
    if text.is_empty() {
        return vec![DisplaySegment {
            logical_line_1based: slice.first_line_number.map(|n| n + 1),
            exact_file_byte_range: (!slice.lossy)
                .then_some(slice.byte_range.start..slice.byte_range.start),
            is_continuation: false,
            text_range: 0..0,
        }];
    }

    let mut segments = Vec::with_capacity(64);
    let mut current_logical_line = slice.first_line_number.map(|n| n + 1);
    let base_file_byte = slice.byte_range.start;
    let max_file_byte = slice.byte_range.end;
    let map_exact_range = |abs_start: usize, abs_end: usize| -> Option<Range<u64>> {
        (!slice.lossy).then(|| {
            let s = (base_file_byte + abs_start as u64).min(max_file_byte);
            let e = (base_file_byte + abs_end as u64).min(max_file_byte);
            s..e
        })
    };
    let bytes = text.as_bytes();
    let mut line_start = 0usize;

    while line_start < bytes.len() {
        let newline_rel = memchr_newline(&bytes[line_start..]);
        let (content_end, next_line_start, has_newline) = match newline_rel {
            Some(rel) => {
                let nl_pos = line_start + rel;
                let content_end = if nl_pos > line_start && bytes[nl_pos - 1] == b'\r' {
                    nl_pos - 1
                } else {
                    nl_pos
                };
                (content_end, nl_pos + 1, true)
            }
            None => (bytes.len(), bytes.len(), false),
        };

        if content_end == line_start {
            segments.push(DisplaySegment {
                logical_line_1based: current_logical_line,
                exact_file_byte_range: map_exact_range(line_start, line_start),
                is_continuation: false,
                text_range: line_start..line_start,
            });
        } else {
            let line_str = &text[line_start..content_end];
            let mut seg_start_in_line = 0usize;
            let mut char_count = 0usize;
            let mut first_seg = true;

            for (byte_idx, _) in line_str.char_indices() {
                if char_count == MAX_DISPLAY_SEGMENT_CHARS {
                    let abs_start = line_start + seg_start_in_line;
                    let abs_end = line_start + byte_idx;
                    segments.push(DisplaySegment {
                        logical_line_1based: if first_seg {
                            current_logical_line
                        } else {
                            None
                        },
                        exact_file_byte_range: map_exact_range(abs_start, abs_end),
                        is_continuation: !first_seg,
                        text_range: abs_start..abs_end,
                    });
                    seg_start_in_line = byte_idx;
                    char_count = 0;
                    first_seg = false;
                }
                char_count += 1;
            }

            let abs_start = line_start + seg_start_in_line;
            let abs_end = content_end;
            segments.push(DisplaySegment {
                logical_line_1based: if first_seg {
                    current_logical_line
                } else {
                    None
                },
                exact_file_byte_range: map_exact_range(abs_start, abs_end),
                is_continuation: !first_seg,
                text_range: abs_start..abs_end,
            });
        }

        if has_newline {
            if let Some(ln) = &mut current_logical_line {
                *ln = ln.saturating_add(1);
            }
        }
        line_start = next_line_start;
    }

    segments
}

#[inline]
fn memchr_newline(bytes: &[u8]) -> Option<usize> {
    bytes.iter().position(|&b| b == b'\n')
}

/// Per-document authority shared across all viewports attached to the same backing file (`§6`,
/// `B5`): owns the sole `PagedDocHandle`, cooperative sparse-indexing lifecycle, and source-wide
/// invalidation state.
pub struct PagedSourceState {
    pub doc_id: DocumentId,
    pub lifecycle_gen: LifecycleGen,
    pub meta: DocMeta,
    pub handle: PagedDocHandle,
    pub source_gen: SourceGeneration,
    pub readonly_reason: ReadOnlyReason,
    /// Present when an ordinary-sized disk file was routed to Paged View due to the 256 MiB /
    /// 128-editor hydrated admission budget (`§15.2`).
    pub admission_note: Option<String>,
    pub index_status: IndexStatus,
    pub source_error: Option<String>,
    pub index_cancel: CancelToken,
}

impl Drop for PagedSourceState {
    fn drop(&mut self) {
        self.index_cancel.cancel();
    }
}

impl PagedSourceState {
    pub fn new(
        doc_id: DocumentId,
        lifecycle_gen: LifecycleGen,
        mut meta: DocMeta,
        handle: PagedDocHandle,
        source_gen: SourceGeneration,
        readonly_reason: ReadOnlyReason,
        admission_note: Option<String>,
    ) -> Self {
        meta.paged = true;
        meta.readonly_reason = Some(readonly_reason.clone());
        let index_status = handle.index_status();
        Self {
            doc_id,
            lifecycle_gen,
            meta,
            handle,
            source_gen,
            readonly_reason,
            admission_note,
            index_status,
            source_error: None,
            index_cancel: CancelToken::new(),
        }
    }

    pub fn index_cancel_token(&self) -> CancelToken {
        self.index_cancel.clone()
    }

    pub fn total_bytes(&self) -> u64 {
        self.handle.len()
    }

    pub fn end_page_offset(&self) -> u64 {
        self.total_bytes().saturating_sub(VIEWPORT_BYTES as u64 / 2)
    }

    pub fn reason_banner(&self) -> String {
        if let Some(note) = &self.admission_note {
            return note.clone();
        }
        #[allow(unreachable_patterns)]
        match self.readonly_reason {
            ReadOnlyReason::LargeFile => format!(
                "Paged Read-Only Mode ({}) — bounded 64 KiB viewport & global 64 MiB cache; full-file Editor load disabled.",
                format_bytes(self.total_bytes())
            ),
            ReadOnlyReason::LongLine => {
                "Paged Read-Only Mode — file contains ultra-long lines (> 1 MiB); bounded 64 KiB fragment display enabled."
                    .to_string()
            }
            ReadOnlyReason::UnsupportedEncoding => {
                "Paged Read-Only Mode — non-UTF-8 / unsupported encoding; editing disabled to prevent silent data corruption."
                    .to_string()
            }
            ReadOnlyReason::MixedLineEndings => {
                "Paged Read-Only Mode — mixed LF/CRLF/CR line endings; editing disabled to preserve exact bytes."
                    .to_string()
            }
            ReadOnlyReason::BinaryContent => {
                "Paged Read-Only Mode — binary / NUL-byte content detected; bounded lossy preview shown."
                    .to_string()
            }
            _ => "Paged Read-Only Mode — bounded read-only preview shown.".to_string(),
        }
    }

    pub fn encoding_label(&self) -> &'static str {
        match self.meta.encoding {
            EncodingSpec::Utf8 => "UTF-8 (prefix)",
            EncodingSpec::Utf8Bom => "UTF-8 BOM",
            EncodingSpec::UnsupportedReadOnly => "Non-UTF-8 [RO]",
        }
    }

    pub fn eol_label(&self) -> &'static str {
        match self.meta.line_ending {
            LineEnding::Lf => "LF (prefix)",
            LineEnding::Crlf => "CRLF (prefix)",
            LineEnding::MixedReadOnly => "Mixed EOL [RO]",
        }
    }
}

/// Outcome of applying an async viewport read to a [`PagedViewportState`].
#[derive(Clone, Debug)]
pub enum PagedViewportApplyOutcome {
    Ignored,
    Applied(IndexStatus),
    SourceChanged(String),
    Failed(String),
}

/// Per-viewport state owned independently by each pane/view attached to a shared
/// [`PagedSourceState`]. Dropping a `PagedViewportState` cancels ONLY its own in-flight
/// `viewport_cancel` token and never cancels `PagedSourceState::index_cancel`.
pub struct PagedViewportState {
    pub current_anchor_byte: u64,
    pub current_slice: Option<ViewportSlice>,
    pub cached_segments: Arc<Vec<DisplaySegment>>,
    pub segment_item_sizes: Rc<Vec<Size<Pixels>>>,
    pub scroll_handle: VirtualListScrollHandle,
    pub highlighted_byte_range: Option<Range<u64>>,
    pub viewport_error: Option<String>,
    pub is_loading: bool,
    pub viewport_req_seq: u64,
    pub viewport_cancel: Option<CancelToken>,
}

impl Drop for PagedViewportState {
    fn drop(&mut self) {
        if let Some(cancel) = self.viewport_cancel.take() {
            cancel.cancel();
        }
    }
}

impl PagedViewportState {
    pub fn new(initial_byte_offset: u64, total_bytes: u64) -> Self {
        let clamped_offset = initial_byte_offset.min(total_bytes);
        Self {
            current_anchor_byte: clamped_offset,
            current_slice: None,
            cached_segments: Arc::new(Vec::new()),
            segment_item_sizes: Rc::new(Vec::new()),
            scroll_handle: VirtualListScrollHandle::new(),
            highlighted_byte_range: None,
            viewport_error: None,
            is_loading: false,
            viewport_req_seq: 0,
            viewport_cancel: None,
        }
    }

    pub fn prepare_viewport_request(
        &mut self,
        source_gen: SourceGeneration,
        anchor: ViewportAnchor,
    ) -> (u64, ViewportRequest, CancelToken) {
        if let Some(prev) = self.viewport_cancel.take() {
            prev.cancel();
        }
        self.viewport_req_seq = self.viewport_req_seq.wrapping_add(1);
        let cancel = CancelToken::new();
        self.viewport_cancel = Some(cancel.clone());
        self.is_loading = true;
        self.viewport_error = None;
        let req = ViewportRequest {
            source_gen,
            anchor,
            max_bytes: VIEWPORT_BYTES,
        };
        (self.viewport_req_seq, req, cancel)
    }

    pub fn apply_viewport_result(
        &mut self,
        seq: u64,
        expected_source_gen: SourceGeneration,
        result: Result<ViewportSlice, ServiceError>,
    ) -> PagedViewportApplyOutcome {
        if seq != self.viewport_req_seq {
            return PagedViewportApplyOutcome::Ignored;
        }
        self.is_loading = false;
        self.viewport_cancel = None;
        match result {
            Ok(slice) => {
                if slice.source_gen != expected_source_gen {
                    let msg = "Underlying file changed on disk; reload to inspect updated content."
                        .to_string();
                    self.viewport_error = Some(msg.clone());
                    return PagedViewportApplyOutcome::SourceChanged(msg);
                }
                self.current_anchor_byte = slice.byte_range.start;
                let index_status = slice.index_status;
                let segs = build_display_segments(&slice);
                let row_size = size(px(SEGMENT_ROW_MEASURE_WIDTH_PX), px(SEGMENT_ROW_HEIGHT_PX));
                self.segment_item_sizes = Rc::new(vec![row_size; segs.len()]);
                self.cached_segments = Arc::new(segs);
                self.scroll_handle.scroll_to_item(0, ScrollStrategy::Top);
                self.current_slice = Some(slice);
                self.viewport_error = None;
                PagedViewportApplyOutcome::Applied(index_status)
            }
            Err(err) => {
                if err.kind == ErrorKind::Cancelled {
                    return PagedViewportApplyOutcome::Ignored;
                }
                self.viewport_error = Some(err.message.clone());
                if err.kind == ErrorKind::SourceChanged {
                    PagedViewportApplyOutcome::SourceChanged(err.message)
                } else {
                    PagedViewportApplyOutcome::Failed(err.message)
                }
            }
        }
    }

    pub fn visible_byte_range(&self) -> Range<u64> {
        self.current_slice
            .as_ref()
            .map(|s| s.byte_range.clone())
            .unwrap_or(self.current_anchor_byte..self.current_anchor_byte)
    }

    pub fn next_page_offset(&self, total_bytes: u64) -> u64 {
        if let Some(slice) = &self.current_slice {
            if slice.byte_range.end >= total_bytes {
                return slice.byte_range.start;
            }
            slice
                .byte_range
                .start
                .saturating_add(PAGE_STEP_BYTES)
                .min(total_bytes.saturating_sub(1))
        } else {
            self.current_anchor_byte
                .saturating_add(PAGE_STEP_BYTES)
                .min(total_bytes)
        }
    }

    pub fn prev_page_offset(&self) -> u64 {
        self.current_anchor_byte.saturating_sub(PAGE_STEP_BYTES)
    }

    pub fn capture_view_state(&self) -> ViewState {
        ViewState {
            cursor_byte: self.current_anchor_byte,
            scroll_byte: self.current_anchor_byte,
        }
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GiB", b / GIB)
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", b / MIB)
    } else if bytes >= 1024 {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Parses a user jump input:
/// - `"b:4096"`, `"@4096"`, `"0x1000"` -> `ByteOffset(u64)` (full 64-bit > 4 GiB supported)
/// - `"l:120"`, `":120"` -> `ApproxLine(119)` (1-based user line -> 0-based `ViewportAnchor::ApproxLine`)
/// - Plain integer `"500"`: in Paged mode defaults to byte offset if `prefer_bytes` is true, else 1-based line!
pub fn parse_jump_query(raw: &str, prefer_bytes: bool) -> Result<ViewportAnchor, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Enter a byte offset (e.g. b:1048576 or 0x100000) or line (:120)".to_string());
    }

    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        let clean = hex.replace('_', "");
        let byte = u64::from_str_radix(&clean, 16)
            .map_err(|_| format!("Invalid hex byte offset '{trimmed}'"))?;
        return Ok(ViewportAnchor::ByteOffset(byte));
    }

    if let Some(rest) = trimmed
        .strip_prefix("b:")
        .or_else(|| trimmed.strip_prefix("B:"))
        .or_else(|| trimmed.strip_prefix('@'))
    {
        let clean = rest.trim().replace('_', "");
        let byte: u64 = clean
            .parse()
            .map_err(|_| format!("Invalid 64-bit byte offset '{rest}'"))?;
        return Ok(ViewportAnchor::ByteOffset(byte));
    }

    if let Some(rest) = trimmed
        .strip_prefix("l:")
        .or_else(|| trimmed.strip_prefix("L:"))
        .or_else(|| trimmed.strip_prefix(':'))
    {
        let clean = rest.trim().replace('_', "");
        let line_1based: u64 = clean
            .parse()
            .map_err(|_| format!("Invalid line number '{rest}'"))?;
        if line_1based == 0 {
            return Err("Line numbers are 1-based (>= 1)".to_string());
        }
        return Ok(ViewportAnchor::ApproxLine(line_1based - 1));
    }

    let clean = trimmed.replace('_', "");
    let value: u64 = clean.parse().map_err(|_| {
        format!("Invalid jump target '{trimmed}'; use b:<byte>, 0x<hex>, or :<line>")
    })?;
    if prefer_bytes {
        Ok(ViewportAnchor::ByteOffset(value))
    } else {
        if value == 0 {
            return Err("Line numbers are 1-based (>= 1); use b:0 for byte 0".to_string());
        }
        Ok(ViewportAnchor::ApproxLine(value - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_long_single_line_without_advancing_logical_line_number() {
        let long_text: Arc<str> = Arc::from("x".repeat(MAX_DISPLAY_SEGMENT_CHARS * 2 + 25));
        let slice = ViewportSlice {
            source_gen: SourceGeneration(1),
            byte_range: 1000..(1000 + long_text.len() as u64),
            text: long_text,
            first_line_number: Some(41), // 0-based -> line 42
            is_line_fragment: true,
            lossy: false,
            index_status: IndexStatus::Complete {
                total_bytes: 5000,
                total_lines: 50,
            },
        };

        let segs = build_display_segments(&slice);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].logical_line_1based, Some(42));
        assert!(!segs[0].is_continuation);
        assert_eq!(segs[1].logical_line_1based, None);
        assert!(segs[1].is_continuation);
        assert_eq!(segs[2].logical_line_1based, None);
        assert!(segs[2].is_continuation);
        assert_eq!(
            segs[2].exact_file_byte_range,
            Some(
                (1000 + (MAX_DISPLAY_SEGMENT_CHARS * 2) as u64)
                    ..(1000 + (MAX_DISPLAY_SEGMENT_CHARS * 2 + 25) as u64)
            )
        );
    }

    #[test]
    fn lossy_viewport_slice_never_claims_exact_row_file_offsets() {
        // Simulate 4 invalid bytes expanding to 4 * 3 = 12 UTF-8 bytes of '\u{FFFD}' + "\nabc"
        let lossy_text: Arc<str> = Arc::from("\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\nabc");
        let slice = ViewportSlice {
            source_gen: SourceGeneration(2),
            byte_range: 500..508, // raw file had only 8 bytes, while decoded text has 16 bytes!
            text: lossy_text,
            first_line_number: Some(0),
            is_line_fragment: false,
            lossy: true,
            index_status: IndexStatus::Complete {
                total_bytes: 508,
                total_lines: 2,
            },
        };

        let segs = build_display_segments(&slice);
        assert_eq!(segs.len(), 2);
        assert!(segs.iter().all(|s| s.exact_file_byte_range.is_none()));
    }

    #[test]
    fn parses_64bit_byte_offsets_above_4gib() {
        let anchor = parse_jump_query("b:5368709120", false).expect("5 GiB offset");
        assert!(matches!(anchor, ViewportAnchor::ByteOffset(5_368_709_120)));

        let hex_anchor = parse_jump_query("0x1_4000_0000", true).expect("5 GiB hex");
        assert!(matches!(
            hex_anchor,
            ViewportAnchor::ByteOffset(0x1_4000_0000)
        ));

        let line_anchor = parse_jump_query(":250", true).expect("line 250");
        assert!(matches!(line_anchor, ViewportAnchor::ApproxLine(249)));
    }

    #[test]
    fn segments_dense_64kib_short_lines_without_allocating_strings() {
        let dense_text: Arc<str> = Arc::from("a\n".repeat(32_768));
        let slice = ViewportSlice {
            source_gen: SourceGeneration(3),
            byte_range: 0..65_536,
            text: dense_text,
            first_line_number: Some(0),
            is_line_fragment: false,
            lossy: false,
            index_status: IndexStatus::Complete {
                total_bytes: 65_536,
                total_lines: 32_768,
            },
        };

        let segs = build_display_segments(&slice);
        // "a\n" repeated 32_768 times ends with '\n', so line_start reaches bytes.len() cleanly.
        assert_eq!(segs.len(), 32_768);
        assert_eq!(segs[0].logical_line_1based, Some(1));
        assert_eq!(segs[32_767].logical_line_1based, Some(32_768));
    }

    #[test]
    fn paged_viewport_state_clamps_offset_and_handles_supersession_and_outcomes() {
        let mut vp = PagedViewportState::new(999, 100);
        assert_eq!(vp.current_anchor_byte, 100);
        assert_eq!(vp.visible_byte_range(), 100..100);
        assert_eq!(
            vp.capture_view_state(),
            ViewState {
                cursor_byte: 100,
                scroll_byte: 100,
            }
        );

        let source_gen = SourceGeneration(7);
        let (seq1, req1, cancel1) =
            vp.prepare_viewport_request(source_gen, ViewportAnchor::ByteOffset(10));
        assert_eq!(seq1, 1);
        assert_eq!(req1.source_gen, source_gen);
        assert_eq!(req1.max_bytes, VIEWPORT_BYTES);
        assert!(vp.is_loading);
        assert!(!cancel1.is_cancelled());

        let (seq2, _req2, cancel2) =
            vp.prepare_viewport_request(source_gen, ViewportAnchor::ByteOffset(20));
        assert_eq!(seq2, 2);
        assert!(cancel1.is_cancelled());
        assert!(!cancel2.is_cancelled());

        // Stale seq1 completion must be ignored without disturbing active seq2.
        assert!(matches!(
            vp.apply_viewport_result(
                seq1,
                source_gen,
                Err(ServiceError::new(ErrorKind::Cancelled, "superseded"))
            ),
            PagedViewportApplyOutcome::Ignored
        ));
        assert!(vp.is_loading);

        // Mismatched slice source generation reports SourceChanged.
        let mismatched_slice = ViewportSlice {
            source_gen: SourceGeneration(8),
            byte_range: 20..30,
            text: Arc::from("0123456789"),
            first_line_number: Some(0),
            is_line_fragment: false,
            lossy: false,
            index_status: IndexStatus::Complete {
                total_bytes: 100,
                total_lines: 1,
            },
        };
        assert!(matches!(
            vp.apply_viewport_result(seq2, source_gen, Ok(mismatched_slice)),
            PagedViewportApplyOutcome::SourceChanged(_)
        ));
        assert!(!vp.is_loading);
        assert!(vp.viewport_error.is_some());

        // Explicit SourceChanged ServiceError maps to SourceChanged.
        let (seq3, _, _) = vp.prepare_viewport_request(source_gen, ViewportAnchor::ByteOffset(0));
        assert!(matches!(
            vp.apply_viewport_result(
                seq3,
                source_gen,
                Err(ServiceError::new(ErrorKind::SourceChanged, "file modified"))
            ),
            PagedViewportApplyOutcome::SourceChanged(msg) if msg == "file modified"
        ));

        // Non-cancel, non-SourceChanged ServiceError maps to Failed.
        let (seq4, _, _) = vp.prepare_viewport_request(source_gen, ViewportAnchor::ByteOffset(0));
        assert!(matches!(
            vp.apply_viewport_result(
                seq4,
                source_gen,
                Err(ServiceError::new(ErrorKind::Io, "io boom"))
            ),
            PagedViewportApplyOutcome::Failed(msg) if msg == "io boom"
        ));

        // Valid slice updates anchor, cached segments, and clears viewport_error.
        let (seq5, _, _) = vp.prepare_viewport_request(source_gen, ViewportAnchor::ByteOffset(0));
        let ok_slice = ViewportSlice {
            source_gen,
            byte_range: 10..22,
            text: Arc::from("hello\nworld\n"),
            first_line_number: Some(1),
            is_line_fragment: false,
            lossy: false,
            index_status: IndexStatus::Complete {
                total_bytes: 100_000,
                total_lines: 500,
            },
        };
        assert!(matches!(
            vp.apply_viewport_result(seq5, source_gen, Ok(ok_slice)),
            PagedViewportApplyOutcome::Applied(IndexStatus::Complete {
                total_bytes: 100_000,
                total_lines: 500,
            })
        ));
        assert!(vp.viewport_error.is_none());
        assert_eq!(vp.current_anchor_byte, 10);
        assert_eq!(vp.visible_byte_range(), 10..22);
        assert_eq!(vp.cached_segments.len(), 2);
        assert_eq!(vp.segment_item_sizes.len(), 2);
        assert_eq!(vp.next_page_offset(100_000), 10 + PAGE_STEP_BYTES);
        assert_eq!(vp.prev_page_offset(), 0);
    }

    #[test]
    fn paged_source_and_viewport_drop_isolation() {
        use std::io::Write as _;

        let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
        writeln!(tmp, "line 1\nline 2\nline 3").expect("write temp");
        tmp.flush().expect("flush temp");

        let paging = editor_services::PagingService::with_budgets(1024 * 1024, 64 * 1024);
        let doc_id = DocumentId::new();
        let handle = paging.open(doc_id, tmp.path()).expect("open paged handle");
        let source_gen = handle.source_generation();

        let source = PagedSourceState::new(
            doc_id,
            LifecycleGen(1),
            DocMeta::default(),
            handle,
            source_gen,
            ReadOnlyReason::LargeFile,
            None,
        );
        let index_cancel = source.index_cancel_token();
        assert!(!index_cancel.is_cancelled());
        assert!(source.total_bytes() > 0);
        assert_eq!(source.end_page_offset(), 0);
        assert!(source.reason_banner().contains("Paged Read-Only Mode"));
        assert_eq!(source.encoding_label(), "UTF-8 (prefix)");
        assert_eq!(source.eol_label(), "LF (prefix)");

        let mut vp1 = PagedViewportState::new(0, source.total_bytes());
        let mut vp2 = PagedViewportState::new(0, source.total_bytes());
        let (_s1, _r1, cancel1) =
            vp1.prepare_viewport_request(source.source_gen, ViewportAnchor::ByteOffset(0));
        let (_s2, _r2, cancel2) =
            vp2.prepare_viewport_request(source.source_gen, ViewportAnchor::ByteOffset(0));

        drop(vp1);
        assert!(cancel1.is_cancelled());
        assert!(!cancel2.is_cancelled());
        assert!(!index_cancel.is_cancelled());

        drop(vp2);
        assert!(cancel2.is_cancelled());
        assert!(!index_cancel.is_cancelled());

        drop(source);
        assert!(index_cancel.is_cancelled());
    }
}
