use crate::{
    CancelToken, ErrorKind, FileFingerprint, PagedDocHandle, ServiceError, SourceGeneration,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const MAX_IMAGE_FILE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_IMAGE_DIMENSION: u32 = 8_192;
pub const MAX_IMAGE_FRAMES: u32 = 128;
pub const MAX_IMAGE_DECODED_BYTES: u64 = 128 * 1024 * 1024;

const HEADER_PREFIX_BYTES: usize = 64 * 1024;
const SMALL_PROBE_WINDOW_BYTES: usize = 4 * 1024;
const GIF_PROBE_WINDOW_BYTES: usize = 64 * 1024;
const MAX_JPEG_HEADER_SCAN_BYTES: u64 = 2 * 1024 * 1024;
const MAX_JPEG_MARKERS: usize = 4096;
const MAX_WEBP_CHUNKS: usize = 4096;
const MAX_TIFF_IFD_ENTRIES: usize = 512;
const READ_CHUNK_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RasterImageFormat {
    Png,
    Jpeg,
    Webp,
    Gif,
    Bmp,
    Ico,
    Tiff,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageHeaderInfo {
    pub format: RasterImageFormat,
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    pub file_bytes: u64,
    pub estimated_decoded_bytes: u64,
}

#[derive(Clone, Debug)]
pub enum ImageOpenOutcome {
    Ready {
        info: ImageHeaderInfo,
        bytes: Arc<[u8]>,
        fingerprint: FileFingerprint,
    },
    OverBudget {
        info: ImageHeaderInfo,
        reason: String,
        fingerprint: FileFingerprint,
    },
}

pub fn matches_raster_magic(prefix: &[u8]) -> bool {
    prefix.starts_with(b"\x89PNG\r\n\x1a\n")
        || prefix.starts_with(&[0xff, 0xd8, 0xff])
        || (prefix.len() >= 12 && &prefix[..4] == b"RIFF" && &prefix[8..12] == b"WEBP")
        || prefix.starts_with(b"GIF87a")
        || prefix.starts_with(b"GIF89a")
        || prefix.starts_with(b"BM")
        || prefix.starts_with(&[0x00, 0x00, 0x01, 0x00])
        || prefix.starts_with(b"II*\x00")
        || prefix.starts_with(b"MM\x00*")
}

trait ProbeRead {
    fn file_bytes(&self) -> u64;
    fn read_at_bounded(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, ServiceError>;
    fn check_cancel(&self) -> Result<(), ServiceError>;
}

struct SliceProbe {
    file_bytes: u64,
}

impl ProbeRead for SliceProbe {
    fn file_bytes(&self) -> u64 {
        self.file_bytes
    }

    fn read_at_bounded(&mut self, _offset: u64, _buf: &mut [u8]) -> Result<usize, ServiceError> {
        Ok(0)
    }

    fn check_cancel(&self) -> Result<(), ServiceError> {
        Ok(())
    }
}

struct PagedDocProbe<'a> {
    doc: &'a PagedDocHandle,
    source_gen: SourceGeneration,
    cancel: &'a CancelToken,
}

impl ProbeRead for PagedDocProbe<'_> {
    fn file_bytes(&self) -> u64 {
        self.doc.len()
    }

    fn read_at_bounded(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, ServiceError> {
        self.cancel.check()?;
        self.doc.read_at(offset, buf, self.source_gen)
    }

    fn check_cancel(&self) -> Result<(), ServiceError> {
        self.cancel.check()
    }
}

struct ProbeCursor<'a, R: ProbeRead> {
    prefix: &'a [u8],
    reader: &'a mut R,
    window: Vec<u8>,
    window_offset: u64,
    window_cap: usize,
}

impl<'a, R: ProbeRead> ProbeCursor<'a, R> {
    fn new(prefix: &'a [u8], reader: &'a mut R, window_cap: usize) -> Self {
        Self {
            prefix,
            reader,
            window: Vec::new(),
            window_offset: 0,
            window_cap,
        }
    }

    fn file_bytes(&self) -> u64 {
        self.reader.file_bytes()
    }

    fn check_cancel(&self) -> Result<(), ServiceError> {
        self.reader.check_cancel()
    }

    fn read_exact_at(&mut self, offset: u64, dst: &mut [u8]) -> Result<bool, ServiceError> {
        let Some(end) = offset.checked_add(dst.len() as u64) else {
            return Ok(false);
        };
        if end > self.reader.file_bytes() {
            return Ok(false);
        }
        if end <= self.prefix.len() as u64 {
            let start = offset as usize;
            dst.copy_from_slice(&self.prefix[start..start + dst.len()]);
            return Ok(true);
        }
        let win_end = self.window_offset.saturating_add(self.window.len() as u64);
        if offset >= self.window_offset && end <= win_end {
            let start = (offset - self.window_offset) as usize;
            dst.copy_from_slice(&self.window[start..start + dst.len()]);
            return Ok(true);
        }
        let want =
            (self.reader.file_bytes() - offset).min(self.window_cap.max(dst.len()) as u64) as usize;
        self.window.resize(want, 0);
        let n = self.reader.read_at_bounded(offset, &mut self.window)?;
        self.window.truncate(n);
        self.window_offset = offset;
        if self.window.len() < dst.len() {
            return Ok(false);
        }
        dst.copy_from_slice(&self.window[..dst.len()]);
        Ok(true)
    }

    fn read_u8_at(&mut self, offset: u64) -> Result<Option<u8>, ServiceError> {
        let mut b = [0u8; 1];
        if self.read_exact_at(offset, &mut b)? {
            Ok(Some(b[0]))
        } else {
            Ok(None)
        }
    }
}

fn checked_decoded_bytes(width: u32, height: u32, frames: u32) -> Option<u64> {
    (width as u64)
        .checked_mul(height as u64)?
        .checked_mul(frames as u64)?
        .checked_mul(4)
}

fn build_header_info(
    format: RasterImageFormat,
    width: u32,
    height: u32,
    frames: u32,
    file_bytes: u64,
) -> ImageHeaderInfo {
    let estimated_decoded_bytes = (width as u64)
        .saturating_mul(height as u64)
        .saturating_mul(frames as u64)
        .saturating_mul(4);
    ImageHeaderInfo {
        format,
        width,
        height,
        frames,
        file_bytes,
        estimated_decoded_bytes,
    }
}

fn budget_violation(info: &ImageHeaderInfo) -> Option<String> {
    if info.file_bytes > MAX_IMAGE_FILE_BYTES {
        return Some(format!(
            "image file size ({} bytes) exceeds {} byte preview limit",
            info.file_bytes, MAX_IMAGE_FILE_BYTES
        ));
    }
    if info.width > MAX_IMAGE_DIMENSION || info.height > MAX_IMAGE_DIMENSION {
        return Some(format!(
            "image dimensions ({}x{}) exceed {}x{} preview limit",
            info.width, info.height, MAX_IMAGE_DIMENSION, MAX_IMAGE_DIMENSION
        ));
    }
    if info.frames > MAX_IMAGE_FRAMES {
        return Some(format!(
            "image frame count ({}) exceeds {} frame preview limit",
            info.frames, MAX_IMAGE_FRAMES
        ));
    }
    if checked_decoded_bytes(info.width, info.height, info.frames)
        .is_none_or(|bytes| bytes > MAX_IMAGE_DECODED_BYTES)
    {
        return Some(format!(
            "estimated decoded image memory ({} bytes) exceeds {} byte preview limit",
            info.estimated_decoded_bytes, MAX_IMAGE_DECODED_BYTES
        ));
    }
    None
}

fn probe_with_reader<R: ProbeRead>(
    prefix: &[u8],
    reader: &mut R,
) -> Result<Option<ImageHeaderInfo>, ServiceError> {
    if !matches_raster_magic(prefix) {
        return Ok(None);
    }
    let parsed = if prefix.starts_with(b"\x89PNG\r\n\x1a\n") {
        probe_png(prefix)
    } else if prefix.starts_with(&[0xff, 0xd8, 0xff]) {
        probe_jpeg(prefix, reader)?
    } else if prefix.len() >= 12 && &prefix[..4] == b"RIFF" && &prefix[8..12] == b"WEBP" {
        probe_webp(prefix, reader)?
    } else if prefix.starts_with(b"GIF87a") || prefix.starts_with(b"GIF89a") {
        probe_gif(prefix, reader)?
    } else if prefix.starts_with(b"BM") {
        probe_bmp(prefix)
    } else if prefix.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        probe_ico(prefix, reader.file_bytes())
    } else if prefix.starts_with(b"II*\x00") || prefix.starts_with(b"MM\x00*") {
        probe_tiff(prefix, reader)?
    } else {
        None
    };

    Ok(parsed.map(|(format, width, height, frames)| {
        build_header_info(format, width, height, frames, reader.file_bytes())
    }))
}

fn probe_png(prefix: &[u8]) -> Option<(RasterImageFormat, u32, u32, u32)> {
    if prefix.len() < 29 || !prefix.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let chunk_len = u32::from_be_bytes([prefix[8], prefix[9], prefix[10], prefix[11]]);
    if chunk_len != 13 || &prefix[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes([prefix[16], prefix[17], prefix[18], prefix[19]]);
    let height = u32::from_be_bytes([prefix[20], prefix[21], prefix[22], prefix[23]]);
    if width == 0 || height == 0 {
        return None;
    }
    let mut frames = 1u32;
    let mut pos = 33usize;
    while pos.saturating_add(12) <= prefix.len() {
        let len = u32::from_be_bytes([
            prefix[pos],
            prefix[pos + 1],
            prefix[pos + 2],
            prefix[pos + 3],
        ]) as usize;
        let ctype = &prefix[pos + 4..pos + 8];
        if ctype == b"acTL" && len == 8 && pos.saturating_add(16) <= prefix.len() {
            let num_frames = u32::from_be_bytes([
                prefix[pos + 8],
                prefix[pos + 9],
                prefix[pos + 10],
                prefix[pos + 11],
            ]);
            if num_frames > 0 {
                frames = num_frames;
            }
            break;
        }
        if ctype == b"IDAT" || ctype == b"IEND" {
            break;
        }
        pos = match pos.checked_add(12).and_then(|next| next.checked_add(len)) {
            Some(next) => next,
            None => break,
        };
    }
    Some((RasterImageFormat::Png, width, height, frames))
}

fn probe_jpeg<R: ProbeRead>(
    prefix: &[u8],
    reader: &mut R,
) -> Result<Option<(RasterImageFormat, u32, u32, u32)>, ServiceError> {
    if !prefix.starts_with(&[0xff, 0xd8, 0xff]) {
        return Ok(None);
    }
    let mut cursor = ProbeCursor::new(prefix, reader, SMALL_PROBE_WINDOW_BYTES);
    let file_bytes = cursor.file_bytes();
    let mut pos = 2u64;
    let mut markers = 0usize;
    while markers < MAX_JPEG_MARKERS && pos < MAX_JPEG_HEADER_SCAN_BYTES && pos < file_bytes {
        cursor.check_cancel()?;
        markers += 1;
        let Some(first) = cursor.read_u8_at(pos)? else {
            return Ok(None);
        };
        if first != 0xff {
            return Ok(None);
        }
        let mut marker_pos = pos + 1;
        let mut fill_count = 0usize;
        let marker = loop {
            let Some(b) = cursor.read_u8_at(marker_pos)? else {
                return Ok(None);
            };
            if b != 0xff {
                break b;
            }
            fill_count += 1;
            if fill_count >= 64 {
                return Ok(None);
            }
            marker_pos += 1;
        };
        let pos_after_marker = marker_pos + 1;
        if marker == 0x00 || marker == 0xd8 || marker == 0xd9 || marker == 0xda {
            return Ok(None);
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            pos = pos_after_marker;
            continue;
        }
        let mut len_bytes = [0u8; 2];
        if !cursor.read_exact_at(pos_after_marker, &mut len_bytes)? {
            return Ok(None);
        }
        let seg_len = u16::from_be_bytes(len_bytes);
        if seg_len < 2 {
            return Ok(None);
        }
        let Some(seg_end) = pos_after_marker.checked_add(seg_len as u64) else {
            return Ok(None);
        };
        if seg_end > file_bytes {
            return Ok(None);
        }
        if matches!(
            marker,
            0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf
        ) {
            if seg_len < 8 {
                return Ok(None);
            }
            let mut sof = [0u8; 6];
            if !cursor.read_exact_at(pos_after_marker + 2, &mut sof)? {
                return Ok(None);
            }
            let height = u16::from_be_bytes([sof[1], sof[2]]) as u32;
            let width = u16::from_be_bytes([sof[3], sof[4]]) as u32;
            let components = sof[5];
            if width == 0 || height == 0 || components == 0 {
                return Ok(None);
            }
            return Ok(Some((RasterImageFormat::Jpeg, width, height, 1)));
        }
        pos = seg_end;
    }
    Ok(None)
}

fn probe_webp<R: ProbeRead>(
    prefix: &[u8],
    reader: &mut R,
) -> Result<Option<(RasterImageFormat, u32, u32, u32)>, ServiceError> {
    if prefix.len() < 20 || &prefix[..4] != b"RIFF" || &prefix[8..12] != b"WEBP" {
        return Ok(None);
    }
    let fourcc = &prefix[12..16];
    let chunk_size = u32::from_le_bytes([prefix[16], prefix[17], prefix[18], prefix[19]]);
    match fourcc {
        b"VP8 " => {
            if prefix.len() < 30 {
                return Ok(None);
            }
            if (prefix[20] & 0x01) != 0 || &prefix[23..26] != &[0x9d, 0x01, 0x2a] {
                return Ok(None);
            }
            let width = (u16::from_le_bytes([prefix[26], prefix[27]]) & 0x3fff) as u32;
            let height = (u16::from_le_bytes([prefix[28], prefix[29]]) & 0x3fff) as u32;
            if width == 0 || height == 0 {
                return Ok(None);
            }
            Ok(Some((RasterImageFormat::Webp, width, height, 1)))
        }
        b"VP8L" => {
            if prefix.len() < 25 || prefix[20] != 0x2f {
                return Ok(None);
            }
            let bits = u32::from_le_bytes([prefix[21], prefix[22], prefix[23], prefix[24]]);
            if (bits >> 29) != 0 {
                return Ok(None);
            }
            let width = (bits & 0x3fff) + 1;
            let height = ((bits >> 14) & 0x3fff) + 1;
            Ok(Some((RasterImageFormat::Webp, width, height, 1)))
        }
        b"VP8X" => {
            if prefix.len() < 30 || chunk_size < 10 {
                return Ok(None);
            }
            let flags = prefix[20];
            let width = u32::from_le_bytes([prefix[24], prefix[25], prefix[26], 0]) + 1;
            let height = u32::from_le_bytes([prefix[27], prefix[28], prefix[29], 0]) + 1;
            let animated = (flags & 0x02) != 0;
            if !animated
                || reader.file_bytes() > MAX_IMAGE_FILE_BYTES
                || width > MAX_IMAGE_DIMENSION
                || height > MAX_IMAGE_DIMENSION
                || checked_decoded_bytes(width, height, 1)
                    .is_none_or(|bytes| bytes > MAX_IMAGE_DECODED_BYTES)
            {
                return Ok(Some((RasterImageFormat::Webp, width, height, 1)));
            }
            let mut cursor = ProbeCursor::new(prefix, reader, SMALL_PROBE_WINDOW_BYTES);
            let file_bytes = cursor.file_bytes();
            let padded_first = (chunk_size as u64).saturating_add(1) & !1;
            let mut pos = 20u64.saturating_add(padded_first);
            let mut frames = 0u32;
            let mut chunks = 0usize;
            while chunks < MAX_WEBP_CHUNKS && pos.saturating_add(8) <= file_bytes {
                cursor.check_cancel()?;
                chunks += 1;
                let mut hdr = [0u8; 8];
                if !cursor.read_exact_at(pos, &mut hdr)? {
                    break;
                }
                let size = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as u64;
                if &hdr[..4] == b"ANMF" {
                    frames = frames.saturating_add(1);
                    if frames > MAX_IMAGE_FRAMES
                        || checked_decoded_bytes(width, height, frames)
                            .is_none_or(|bytes| bytes > MAX_IMAGE_DECODED_BYTES)
                    {
                        break;
                    }
                }
                let padded = size.saturating_add(1) & !1;
                let Some(next) = pos.checked_add(8).and_then(|p| p.checked_add(padded)) else {
                    break;
                };
                pos = next;
            }
            Ok(Some((
                RasterImageFormat::Webp,
                width,
                height,
                frames.max(1),
            )))
        }
        _ => Ok(None),
    }
}

fn skip_gif_sub_blocks<R: ProbeRead>(
    cursor: &mut ProbeCursor<'_, R>,
    mut pos: u64,
) -> Result<Option<u64>, ServiceError> {
    let file_bytes = cursor.file_bytes();
    while pos < file_bytes {
        cursor.check_cancel()?;
        let Some(block_size) = cursor.read_u8_at(pos)? else {
            return Ok(None);
        };
        pos += 1;
        if block_size == 0 {
            return Ok(Some(pos));
        }
        let Some(next) = pos.checked_add(block_size as u64) else {
            return Ok(None);
        };
        if next > file_bytes {
            return Ok(None);
        }
        pos = next;
    }
    Ok(None)
}

fn probe_gif<R: ProbeRead>(
    prefix: &[u8],
    reader: &mut R,
) -> Result<Option<(RasterImageFormat, u32, u32, u32)>, ServiceError> {
    if prefix.len() < 13 || (!prefix.starts_with(b"GIF87a") && !prefix.starts_with(b"GIF89a")) {
        return Ok(None);
    }
    let width = u16::from_le_bytes([prefix[6], prefix[7]]) as u32;
    let height = u16::from_le_bytes([prefix[8], prefix[9]]) as u32;
    if width == 0 || height == 0 {
        return Ok(None);
    }
    if reader.file_bytes() > MAX_IMAGE_FILE_BYTES
        || width > MAX_IMAGE_DIMENSION
        || height > MAX_IMAGE_DIMENSION
        || checked_decoded_bytes(width, height, 1)
            .is_none_or(|bytes| bytes > MAX_IMAGE_DECODED_BYTES)
    {
        return Ok(Some((RasterImageFormat::Gif, width, height, 1)));
    }
    let packed = prefix[10];
    let mut pos = 13u64;
    if (packed & 0x80) != 0 {
        let gct_bytes = 3u64 << ((packed & 0x07) + 1);
        pos = pos.saturating_add(gct_bytes);
    }
    let mut cursor = ProbeCursor::new(prefix, reader, GIF_PROBE_WINDOW_BYTES);
    let file_bytes = cursor.file_bytes();
    let mut frames = 0u32;
    while pos < file_bytes {
        cursor.check_cancel()?;
        let Some(introducer) = cursor.read_u8_at(pos)? else {
            break;
        };
        match introducer {
            0x3b => break,
            0x21 => {
                if pos + 2 > file_bytes {
                    break;
                }
                let Some(next) = skip_gif_sub_blocks(&mut cursor, pos + 2)? else {
                    break;
                };
                pos = next;
            }
            0x2c => {
                let mut desc = [0u8; 10];
                if !cursor.read_exact_at(pos, &mut desc)? {
                    break;
                }
                frames = frames.saturating_add(1);
                if frames > MAX_IMAGE_FRAMES
                    || checked_decoded_bytes(width, height, frames)
                        .is_none_or(|bytes| bytes > MAX_IMAGE_DECODED_BYTES)
                {
                    break;
                }
                let lct_packed = desc[9];
                pos += 10;
                if (lct_packed & 0x80) != 0 {
                    pos = pos.saturating_add(3u64 << ((lct_packed & 0x07) + 1));
                }
                if pos >= file_bytes {
                    break;
                }
                pos += 1; // LZW minimum code size.
                let Some(next) = skip_gif_sub_blocks(&mut cursor, pos)? else {
                    break;
                };
                pos = next;
            }
            _ => break,
        }
    }
    if frames == 0 {
        return Ok(None);
    }
    Ok(Some((RasterImageFormat::Gif, width, height, frames)))
}

fn probe_bmp(prefix: &[u8]) -> Option<(RasterImageFormat, u32, u32, u32)> {
    if !prefix.starts_with(b"BM") || prefix.len() < 26 {
        return None;
    }
    let dib_size = u32::from_le_bytes([prefix[14], prefix[15], prefix[16], prefix[17]]);
    let (width, height) = if dib_size == 12 {
        let width = u16::from_le_bytes([prefix[18], prefix[19]]) as u32;
        let height = u16::from_le_bytes([prefix[20], prefix[21]]) as u32;
        let planes = u16::from_le_bytes([prefix[22], prefix[23]]);
        let bpp = u16::from_le_bytes([prefix[24], prefix[25]]);
        if planes != 1 || !matches!(bpp, 1 | 4 | 8 | 24) {
            return None;
        }
        (width, height)
    } else if matches!(dib_size, 40 | 52 | 56 | 64 | 108 | 124) {
        if prefix.len() < 14 + dib_size as usize {
            return None;
        }
        let (width, height) = if dib_size == 64 {
            (
                u32::from_le_bytes([prefix[18], prefix[19], prefix[20], prefix[21]]),
                u32::from_le_bytes([prefix[22], prefix[23], prefix[24], prefix[25]]),
            )
        } else {
            let raw_w = i32::from_le_bytes([prefix[18], prefix[19], prefix[20], prefix[21]]);
            let raw_h = i32::from_le_bytes([prefix[22], prefix[23], prefix[24], prefix[25]]);
            if raw_w <= 0 || raw_h == 0 {
                return None;
            }
            (raw_w as u32, raw_h.unsigned_abs())
        };
        let planes = u16::from_le_bytes([prefix[26], prefix[27]]);
        let bpp = u16::from_le_bytes([prefix[28], prefix[29]]);
        if planes != 1 || !matches!(bpp, 0 | 1 | 2 | 4 | 8 | 16 | 24 | 32 | 64) {
            return None;
        }
        (width, height)
    } else {
        return None;
    };
    if width == 0 || height == 0 {
        return None;
    }
    Some((RasterImageFormat::Bmp, width, height, 1))
}

fn probe_ico(prefix: &[u8], file_bytes: u64) -> Option<(RasterImageFormat, u32, u32, u32)> {
    if !prefix.starts_with(&[0x00, 0x00, 0x01, 0x00]) || prefix.len() < 22 {
        return None;
    }
    let count = u16::from_le_bytes([prefix[4], prefix[5]]) as usize;
    if count == 0 || file_bytes < 6 + (count as u64) * 16 {
        return None;
    }
    let inspect_count = count.min(256);
    if prefix.len() < 6 + inspect_count * 16 {
        return None;
    }
    let mut best_w = 0u32;
    let mut best_h = 0u32;
    let mut best_area = 0u64;
    for i in 0..inspect_count {
        let entry = &prefix[6 + i * 16..6 + (i + 1) * 16];
        let mut w = if entry[0] == 0 { 256 } else { entry[0] as u32 };
        let mut h = if entry[1] == 0 { 256 } else { entry[1] as u32 };
        let img_size = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as usize;
        let img_offset = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]) as usize;
        if img_size >= 29
            && let Some(end) = img_offset.checked_add(29)
            && end <= prefix.len()
            && let Some((_, pw, ph, _)) = probe_png(&prefix[img_offset..])
        {
            w = pw;
            h = ph;
        }
        let area = (w as u64).saturating_mul(h as u64);
        if area > best_area {
            best_area = area;
            best_w = w;
            best_h = h;
        }
    }
    if best_w == 0 || best_h == 0 {
        return None;
    }
    Some((RasterImageFormat::Ico, best_w, best_h, 1))
}

fn probe_tiff<R: ProbeRead>(
    prefix: &[u8],
    reader: &mut R,
) -> Result<Option<(RasterImageFormat, u32, u32, u32)>, ServiceError> {
    if prefix.len() < 8 {
        return Ok(None);
    }
    let little_endian = if prefix.starts_with(b"II*\x00") {
        true
    } else if prefix.starts_with(b"MM\x00*") {
        false
    } else {
        return Ok(None);
    };
    let read_u16 = |b: &[u8]| -> u16 {
        if little_endian {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        }
    };
    let read_u32 = |b: &[u8]| -> u32 {
        if little_endian {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        }
    };
    let ifd_offset = read_u32(&prefix[4..8]) as u64;
    let file_bytes = reader.file_bytes();
    if ifd_offset < 8 || ifd_offset.saturating_add(2) > file_bytes {
        return Ok(None);
    }
    let mut cursor = ProbeCursor::new(prefix, reader, SMALL_PROBE_WINDOW_BYTES);
    let mut count_bytes = [0u8; 2];
    if !cursor.read_exact_at(ifd_offset, &mut count_bytes)? {
        return Ok(None);
    }
    let num_entries = read_u16(&count_bytes) as usize;
    if num_entries == 0 || num_entries > MAX_TIFF_IFD_ENTRIES {
        return Ok(None);
    }
    let entries_len = num_entries * 12;
    let Some(ifd_end) = ifd_offset.checked_add(2 + entries_len as u64) else {
        return Ok(None);
    };
    if ifd_end > file_bytes {
        return Ok(None);
    }
    let mut entries = vec![0u8; entries_len];
    if !cursor.read_exact_at(ifd_offset + 2, &mut entries)? {
        return Ok(None);
    }
    let mut width = None;
    let mut height = None;
    for i in 0..num_entries {
        let entry = &entries[i * 12..(i + 1) * 12];
        let tag = read_u16(&entry[0..2]);
        let field_type = read_u16(&entry[2..4]);
        let count = read_u32(&entry[4..8]);
        if (tag == 256 || tag == 257) && count == 1 {
            let value = match field_type {
                1 => Some(entry[8] as u32),
                3 => Some(read_u16(&entry[8..10]) as u32),
                4 => Some(read_u32(&entry[8..12])),
                _ => None,
            };
            if tag == 256 {
                width = value;
            } else {
                height = value;
            }
        }
    }
    let (Some(w), Some(h)) = (width, height) else {
        return Ok(None);
    };
    if w == 0 || h == 0 {
        return Ok(None);
    }
    Ok(Some((RasterImageFormat::Tiff, w, h, 1)))
}

pub fn probe_raster_header(prefix: &[u8], file_bytes: u64) -> Option<ImageHeaderInfo> {
    let mut reader = SliceProbe { file_bytes };
    probe_with_reader(prefix, &mut reader).ok().flatten()
}

pub fn open_raster_from_paged_doc(
    doc: &PagedDocHandle,
    source_gen: SourceGeneration,
    fingerprint: FileFingerprint,
    cancel: &CancelToken,
) -> Result<Option<ImageOpenOutcome>, ServiceError> {
    cancel.check()?;
    let file_bytes = doc.len();
    if file_bytes == 0 {
        return Ok(None);
    }
    let initial_len = (file_bytes as usize).min(HEADER_PREFIX_BYTES);
    let mut prefix = vec![0u8; initial_len];
    let n = doc.read_at(0, &mut prefix, source_gen)?;
    prefix.truncate(n);
    if !matches_raster_magic(&prefix) {
        return Ok(None);
    }
    let mut reader = PagedDocProbe {
        doc,
        source_gen,
        cancel,
    };
    let Some(info) = probe_with_reader(&prefix, &mut reader)? else {
        return Ok(None);
    };
    if let Some(reason) = budget_violation(&info) {
        return Ok(Some(ImageOpenOutcome::OverBudget {
            info,
            reason,
            fingerprint,
        }));
    }
    let total = file_bytes as usize;
    let mut bytes = vec![0u8; total];
    let copied = prefix.len().min(total);
    bytes[..copied].copy_from_slice(&prefix[..copied]);
    let mut offset = copied as u64;
    while offset < file_bytes {
        cancel.check()?;
        let start = offset as usize;
        let end = (start + READ_CHUNK_BYTES).min(total);
        let read = doc.read_at(offset, &mut bytes[start..end], source_gen)?;
        if read == 0 {
            doc.validate(source_gen)?;
            return Err(ServiceError::new(
                ErrorKind::SourceChanged,
                "image source ended prematurely",
            ));
        }
        offset += read as u64;
    }
    doc.validate(source_gen)?;
    Ok(Some(ImageOpenOutcome::Ready {
        info,
        bytes: Arc::from(bytes),
        fingerprint,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocumentId, PagingService};
    use std::io::Write;
    use tempfile::TempDir;

    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(33);
        out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(b"IHDR");
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&[8, 6, 0, 0, 0]);
        out.extend_from_slice(&[0u8; 4]);
        out
    }

    fn jpeg_with_app1(app1_payload_len: usize, width: u16, height: u16) -> Vec<u8> {
        let mut out = vec![0xff, 0xd8];
        let mut remaining = app1_payload_len;
        while remaining > 0 {
            let chunk = remaining.min(65_500);
            out.extend_from_slice(&[0xff, 0xe1]);
            out.extend_from_slice(&((chunk + 2) as u16).to_be_bytes());
            out.extend(std::iter::repeat_n(0x41, chunk));
            remaining -= chunk;
        }
        out.extend_from_slice(&[0xff, 0xc0]);
        out.extend_from_slice(&11u16.to_be_bytes());
        out.push(8);
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out.push(3);
        out.extend_from_slice(&[1, 0x11, 0]);
        out.extend_from_slice(&[0xff, 0xd9]);
        out
    }

    fn gif_header(width: u16, height: u16, frames: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"GIF89a");
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.extend_from_slice(&[0x00, 0x00, 0x00]);
        for _ in 0..frames {
            out.push(0x2c);
            out.extend_from_slice(&[0u8; 4]);
            out.extend_from_slice(&width.to_le_bytes());
            out.extend_from_slice(&height.to_le_bytes());
            out.push(0x00);
            out.push(0x02); // LZW min code size
            out.push(0x02); // sub-block length
            out.extend_from_slice(&[0x44, 0x01]);
            out.push(0x00); // block terminator
        }
        out.push(0x3b);
        out
    }

    fn bmp_header(width: i32, height: i32) -> Vec<u8> {
        let mut out = vec![0u8; 54];
        out[0..2].copy_from_slice(b"BM");
        out[2..6].copy_from_slice(&54u32.to_le_bytes());
        out[10..14].copy_from_slice(&54u32.to_le_bytes());
        out[14..18].copy_from_slice(&40u32.to_le_bytes());
        out[18..22].copy_from_slice(&width.to_le_bytes());
        out[22..26].copy_from_slice(&height.to_le_bytes());
        out[26..28].copy_from_slice(&1u16.to_le_bytes());
        out[28..30].copy_from_slice(&24u16.to_le_bytes());
        out
    }

    fn ico_header(entries: &[(u8, u8)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&[0x00, 0x00, 0x01, 0x00]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        let data_offset = 6 + entries.len() * 16;
        for &(w, h) in entries {
            out.push(w);
            out.push(h);
            out.push(0);
            out.push(0);
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&32u16.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&(data_offset as u32).to_le_bytes());
        }
        out
    }

    fn tiff_header(little_endian: bool, width: u32, height: u32, short_fields: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let push_u16 = |buf: &mut Vec<u8>, v: u16| {
            if little_endian {
                buf.extend_from_slice(&v.to_le_bytes());
            } else {
                buf.extend_from_slice(&v.to_be_bytes());
            }
        };
        let push_u32 = |buf: &mut Vec<u8>, v: u32| {
            if little_endian {
                buf.extend_from_slice(&v.to_le_bytes());
            } else {
                buf.extend_from_slice(&v.to_be_bytes());
            }
        };
        if little_endian {
            out.extend_from_slice(b"II*\x00");
        } else {
            out.extend_from_slice(b"MM\x00*");
        }
        push_u32(&mut out, 8);
        push_u16(&mut out, 2);
        for (tag, val) in [(256u16, width), (257u16, height)] {
            push_u16(&mut out, tag);
            if short_fields {
                push_u16(&mut out, 3);
                push_u32(&mut out, 1);
                push_u16(&mut out, val as u16);
                push_u16(&mut out, 0);
            } else {
                push_u16(&mut out, 4);
                push_u32(&mut out, 1);
                push_u32(&mut out, val);
            }
        }
        out
    }

    #[test]
    fn probes_all_seven_raster_formats() {
        let png = png_header(1280, 720);
        let info = probe_raster_header(&png, png.len() as u64).unwrap();
        assert_eq!(
            info,
            ImageHeaderInfo {
                format: RasterImageFormat::Png,
                width: 1280,
                height: 720,
                frames: 1,
                file_bytes: png.len() as u64,
                estimated_decoded_bytes: 1280 * 720 * 4,
            }
        );

        let jpg = jpeg_with_app1(128, 640, 480);
        let info = probe_raster_header(&jpg, jpg.len() as u64).unwrap();
        assert_eq!(info.format, RasterImageFormat::Jpeg);
        assert_eq!((info.width, info.height, info.frames), (640, 480, 1));

        // WebP VP8 (lossy)
        let mut vp8 = Vec::new();
        vp8.extend_from_slice(b"RIFF");
        vp8.extend_from_slice(&30u32.to_le_bytes());
        vp8.extend_from_slice(b"WEBP");
        vp8.extend_from_slice(b"VP8 ");
        vp8.extend_from_slice(&10u32.to_le_bytes());
        vp8.extend_from_slice(&[0x00, 0x00, 0x00, 0x9d, 0x01, 0x2a]);
        vp8.extend_from_slice(&320u16.to_le_bytes());
        vp8.extend_from_slice(&240u16.to_le_bytes());
        let info = probe_raster_header(&vp8, vp8.len() as u64).unwrap();
        assert_eq!(info.format, RasterImageFormat::Webp);
        assert_eq!((info.width, info.height, info.frames), (320, 240, 1));

        // WebP VP8L (lossless)
        let mut vp8l = Vec::new();
        vp8l.extend_from_slice(b"RIFF");
        vp8l.extend_from_slice(&25u32.to_le_bytes());
        vp8l.extend_from_slice(b"WEBP");
        vp8l.extend_from_slice(b"VP8L");
        vp8l.extend_from_slice(&5u32.to_le_bytes());
        vp8l.push(0x2f);
        let bits: u32 = (512 - 1) | ((256 - 1) << 14);
        vp8l.extend_from_slice(&bits.to_le_bytes());
        let info = probe_raster_header(&vp8l, vp8l.len() as u64).unwrap();
        assert_eq!(info.format, RasterImageFormat::Webp);
        assert_eq!((info.width, info.height, info.frames), (512, 256, 1));

        // WebP VP8X (extended)
        let mut vp8x = Vec::new();
        vp8x.extend_from_slice(b"RIFF");
        vp8x.extend_from_slice(&30u32.to_le_bytes());
        vp8x.extend_from_slice(b"WEBP");
        vp8x.extend_from_slice(b"VP8X");
        vp8x.extend_from_slice(&10u32.to_le_bytes());
        vp8x.extend_from_slice(&[0x10, 0, 0, 0]);
        let w = (1024u32 - 1).to_le_bytes();
        let h = (768u32 - 1).to_le_bytes();
        vp8x.extend_from_slice(&w[..3]);
        vp8x.extend_from_slice(&h[..3]);
        let info = probe_raster_header(&vp8x, vp8x.len() as u64).unwrap();
        assert_eq!(info.format, RasterImageFormat::Webp);
        assert_eq!((info.width, info.height, info.frames), (1024, 768, 1));

        // GIF 1-frame & multi-frame
        let gif1 = gif_header(100, 50, 1);
        let info = probe_raster_header(&gif1, gif1.len() as u64).unwrap();
        assert_eq!(info.format, RasterImageFormat::Gif);
        assert_eq!((info.width, info.height, info.frames), (100, 50, 1));

        let gif4 = gif_header(100, 50, 4);
        let info = probe_raster_header(&gif4, gif4.len() as u64).unwrap();
        assert_eq!(info.format, RasterImageFormat::Gif);
        assert_eq!((info.width, info.height, info.frames), (100, 50, 4));
        assert_eq!(info.estimated_decoded_bytes, 100 * 50 * 4 * 4);

        // BMP (positive and top-down negative height)
        let bmp = bmp_header(400, -300);
        let info = probe_raster_header(&bmp, bmp.len() as u64).unwrap();
        assert_eq!(info.format, RasterImageFormat::Bmp);
        assert_eq!((info.width, info.height, info.frames), (400, 300, 1));

        // ICO (0 means 256)
        let ico = ico_header(&[(16, 16), (0, 0)]);
        let info = probe_raster_header(&ico, ico.len() as u64).unwrap();
        assert_eq!(info.format, RasterImageFormat::Ico);
        assert_eq!((info.width, info.height, info.frames), (256, 256, 1));

        // TIFF LE & BE (both SHORT and LONG)
        for (le, short) in [(true, true), (false, true), (true, false), (false, false)] {
            let tiff = tiff_header(le, 800, 600, short);
            let info = probe_raster_header(&tiff, tiff.len() as u64).unwrap();
            assert_eq!(info.format, RasterImageFormat::Tiff);
            assert_eq!((info.width, info.height, info.frames), (800, 600, 1));
        }
    }

    #[test]
    fn jpeg_skips_large_exif_segments_via_bounded_read_at() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("exif.jpg");
        let data = jpeg_with_app1(256 * 1024, 1920, 1080);
        std::fs::write(&path, &data).unwrap();
        let paging = PagingService::default();
        let doc = paging.open(DocumentId::new(), &path).unwrap();
        let outcome = open_raster_from_paged_doc(
            &doc,
            doc.source_generation(),
            doc.fingerprint().clone(),
            &CancelToken::new(),
        )
        .unwrap()
        .unwrap();
        match outcome {
            ImageOpenOutcome::Ready { info, bytes, .. } => {
                assert_eq!(info.format, RasterImageFormat::Jpeg);
                assert_eq!((info.width, info.height, info.frames), (1920, 1080, 1));
                assert_eq!(bytes.len(), data.len());
            }
            ImageOpenOutcome::OverBudget { reason, .. } => {
                panic!("unexpected over-budget: {reason}");
            }
        }
    }

    #[test]
    fn budget_gates_catch_bombs_and_oversized_files_without_reading_body() {
        let dir = TempDir::new().unwrap();
        let paging = PagingService::default();

        // 20000x20000 decompression bomb with 10 MiB body
        let bomb_path = dir.path().join("bomb.png");
        let mut f = std::fs::File::create(&bomb_path).unwrap();
        f.write_all(&png_header(20_000, 20_000)).unwrap();
        f.set_len(10 * 1024 * 1024).unwrap();
        drop(f);
        let doc = paging.open(DocumentId::new(), &bomb_path).unwrap();
        match open_raster_from_paged_doc(
            &doc,
            doc.source_generation(),
            doc.fingerprint().clone(),
            &CancelToken::new(),
        )
        .unwrap()
        .unwrap()
        {
            ImageOpenOutcome::OverBudget { info, reason, .. } => {
                assert_eq!((info.width, info.height), (20_000, 20_000));
                assert!(reason.contains("exceed"));
            }
            ImageOpenOutcome::Ready { .. } => panic!("bomb must be OverBudget"),
        }

        // > 32 MiB PNG file
        let big_path = dir.path().join("big.png");
        let mut f = std::fs::File::create(&big_path).unwrap();
        f.write_all(&png_header(64, 64)).unwrap();
        f.set_len(MAX_IMAGE_FILE_BYTES + 1).unwrap();
        drop(f);
        let doc = paging.open(DocumentId::new(), &big_path).unwrap();
        match open_raster_from_paged_doc(
            &doc,
            doc.source_generation(),
            doc.fingerprint().clone(),
            &CancelToken::new(),
        )
        .unwrap()
        .unwrap()
        {
            ImageOpenOutcome::OverBudget { info, reason, .. } => {
                assert_eq!(info.file_bytes, MAX_IMAGE_FILE_BYTES + 1);
                assert!(reason.contains("exceed"));
            }
            ImageOpenOutcome::Ready { .. } => panic!("oversize file must be OverBudget"),
        }

        // > 32 MiB GIF computes 1-frame memory from LSD without walking frames
        let big_gif_path = dir.path().join("big.gif");
        let mut f = std::fs::File::create(&big_gif_path).unwrap();
        let mut hdr = Vec::new();
        hdr.extend_from_slice(b"GIF89a");
        hdr.extend_from_slice(&320u16.to_le_bytes());
        hdr.extend_from_slice(&200u16.to_le_bytes());
        hdr.extend_from_slice(&[0, 0, 0]);
        f.write_all(&hdr).unwrap();
        f.set_len(MAX_IMAGE_FILE_BYTES + 1024).unwrap();
        drop(f);
        let doc = paging.open(DocumentId::new(), &big_gif_path).unwrap();
        match open_raster_from_paged_doc(
            &doc,
            doc.source_generation(),
            doc.fingerprint().clone(),
            &CancelToken::new(),
        )
        .unwrap()
        .unwrap()
        {
            ImageOpenOutcome::OverBudget { info, .. } => {
                assert_eq!(info.format, RasterImageFormat::Gif);
                assert_eq!((info.width, info.height, info.frames), (320, 200, 1));
            }
            ImageOpenOutcome::Ready { .. } => panic!("oversize GIF must be OverBudget"),
        }
    }

    #[test]
    fn non_images_and_corrupt_headers_never_misclassify() {
        for text in [
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>".as_slice(),
            b"<?xml version=\"1.0\"?><svg/>".as_slice(),
            b"hello plain text\n".as_slice(),
            b"\xef\xbb\xbf<svg/>".as_slice(),
            b"".as_slice(),
        ] {
            assert!(!matches_raster_magic(text));
            assert!(probe_raster_header(text, text.len() as u64).is_none());
        }

        // Zero-dimension or truncated magic returns None so open_document falls back safely.
        assert!(probe_raster_header(&png_header(0, 100), 33).is_none());
        assert!(probe_raster_header(b"\x89PNG\r\n\x1a\n", 8).is_none());
        assert!(probe_raster_header(b"BM plain text file\n", 19).is_none());
    }
}
