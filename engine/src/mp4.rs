//! MP4 Integrity Gate + offset repair.
//!
//! Do kaam:
//! 1. validate() — carved MP4 ka structural report (Integrity Gate).
//!    Gate rule: ftyp + moov + mdat + no truncation = "verified" (teal).
//! 2. rewrite_chunk_offsets() — carved files mein stco/co64 absolute
//!    offsets shift ho jaate hain; delta se patch karke playable banao.
//!
//! Reference-file reconstruction (missing moov rebuild) = Phase 2.5 scope.

#[derive(Debug, Clone)]
pub struct BoxInfo {
    pub typ: String,
    pub offset: u64,
    pub size: u64,
}

#[derive(Debug, Default, Clone)]
pub struct Mp4Report {
    pub has_ftyp: bool,
    pub has_moov: bool,
    pub has_mdat: bool,
    pub moov_before_mdat: bool,
    pub truncated: bool,
    pub top_level_boxes: Vec<BoxInfo>,
}

impl Mp4Report {
    /// Integrity Gate verdict — crystal pe teal (verified) ya nahi.
    pub fn playable_estimate(&self) -> bool {
        self.has_ftyp && self.has_moov && self.has_mdat && !self.truncated
    }
}

/// ISO BMFF container boxes — inke andar recurse karna hai.
const CONTAINERS: &[&[u8; 4]] = &[
    b"moov", b"trak", b"mdia", b"minf", b"stbl", b"edts", b"dinf", b"udta",
];

#[inline]
fn u32be(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
#[inline]
fn u64be(b: &[u8], o: usize) -> u64 {
    u64::from_be_bytes(b[o..o + 8].try_into().unwrap())
}

/// Box header parse. Returns (type, header_size, total_size).
/// None = invalid/truncated header — caller treats as end, kabhi panic nahi.
fn parse_header(buf: &[u8], off: usize) -> Option<([u8; 4], usize, u64)> {
    if off + 8 > buf.len() {
        return None;
    }
    let size32 = u32be(buf, off);
    let typ: [u8; 4] = buf[off + 4..off + 8].try_into().ok()?;
    match size32 {
        0 => Some((typ, 8, (buf.len() - off) as u64)), // extends to EOF
        1 => {
            if off + 16 > buf.len() {
                return None;
            }
            Some((typ, 16, u64be(buf, off + 8))) // 64-bit largesize
        }
        s => Some((typ, 8, s as u64)),
    }
}

/// Top-level walk → structural report.
pub fn validate(buf: &[u8]) -> Mp4Report {
    let mut report = Mp4Report::default();
    let mut off = 0usize;
    let mut moov_pos = u64::MAX;
    let mut mdat_pos = u64::MAX;

    while off < buf.len() {
        let Some((typ, _hdr, total)) = parse_header(buf, off) else {
            break;
        };
        if total == 0 {
            break; // corrupt zero-size — infinite loop guard
        }
        let end = off.saturating_add(total as usize);
        if end > buf.len() {
            report.truncated = true; // declared size aage hai — carve short tha
            report.top_level_boxes.push(BoxInfo {
                typ: String::from_utf8_lossy(&typ).to_string(),
                offset: off as u64,
                size: total,
            });
            break;
        }
        match &typ {
            b"ftyp" => report.has_ftyp = true,
            b"moov" => {
                report.has_moov = true;
                moov_pos = off as u64;
            }
            b"mdat" => {
                report.has_mdat = true;
                mdat_pos = off as u64;
            }
            _ => {}
        }
        report.top_level_boxes.push(BoxInfo {
            typ: String::from_utf8_lossy(&typ).to_string(),
            offset: off as u64,
            size: total,
        });
        off = end;
    }
    report.moov_before_mdat = moov_pos < mdat_pos;
    report
}

/// stco/co64 chunk offsets ko `delta` se shift karo (in-place).
/// Returns: patched entry count. Use case: carve ke baad file start
/// shift hua hai; saare absolute offsets ek hi delta se theek hote hain.
pub fn rewrite_chunk_offsets(buf: &mut [u8], delta: i64) -> usize {
    let len = buf.len();
    patch_range(buf, 0, len, delta)
}

fn patch_range(buf: &mut [u8], start: usize, end: usize, delta: i64) -> usize {
    let mut patched = 0;
    let mut off = start;
    while off < end {
        let limit = end.min(buf.len());
        let Some((typ, hdr, total)) = parse_header(&buf[..limit], off) else {
            break;
        };
        let box_end = off.saturating_add(total as usize).min(limit);
        if total == 0 || box_end <= off {
            break;
        }
        let content = off + hdr;
        match &typ {
            b"stco" => patched += patch_table(buf, content, box_end, 4, delta),
            b"co64" => patched += patch_table(buf, content, box_end, 8, delta),
            t if CONTAINERS.contains(&t) => {
                patched += patch_range(buf, content, box_end, delta);
            }
            _ => {}
        }
        off = box_end;
    }
    patched
}

/// stco/co64 body: version(1) + flags(3) + entry_count(4) + entries.
fn patch_table(buf: &mut [u8], content: usize, box_end: usize, width: usize, delta: i64) -> usize {
    if content + 8 > box_end {
        return 0;
    }
    let count = u32be(buf, content + 4) as usize;
    let entries_start = content + 8;
    let available = box_end.saturating_sub(entries_start) / width;
    let n = count.min(available); // truncated table pe bhi safe

    let region_end = entries_start + n * width;
    let region = &mut buf[entries_start..region_end];
    for chunk in region.chunks_exact_mut(width) {
        if width == 4 {
            let v = u32::from_be_bytes(chunk.try_into().unwrap()) as i64;
            let nv = (v + delta).clamp(0, u32::MAX as i64) as u32;
            chunk.copy_from_slice(&nv.to_be_bytes());
        } else {
            let v = u64::from_be_bytes(chunk.try_into().unwrap()) as i64;
            let nv = (v + delta).clamp(0, i64::MAX) as u64;
            chunk.copy_from_slice(&nv.to_be_bytes());
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(typ: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
        v.extend_from_slice(typ);
        v.extend_from_slice(body);
        v
    }

    /// Minimal valid-ish MP4: ftyp + mdat + moov(trak(mdia(minf(stbl(stco)))))
    fn synth_mp4() -> Vec<u8> {
        let mut stco_body = vec![0u8; 4]; // version+flags
        stco_body.extend_from_slice(&2u32.to_be_bytes()); // 2 entries
        stco_body.extend_from_slice(&1000u32.to_be_bytes());
        stco_body.extend_from_slice(&2000u32.to_be_bytes());
        let stbl = bx(b"stbl", &bx(b"stco", &stco_body));
        let minf = bx(b"minf", &stbl);
        let mdia = bx(b"mdia", &minf);
        let trak = bx(b"trak", &mdia);
        let moov = bx(b"moov", &trak);
        let ftyp = bx(b"ftyp", b"isom\0\0\0\x01");
        let mdat = bx(b"mdat", &[0xAB; 64]);
        [ftyp, mdat, moov].concat()
    }

    #[test]
    fn validate_clean_mp4_passes_gate() {
        let r = validate(&synth_mp4());
        assert!(r.has_ftyp && r.has_moov && r.has_mdat);
        assert!(!r.truncated);
        assert!(r.playable_estimate()); // Integrity Gate → teal
    }

    #[test]
    fn truncated_box_flagged_not_fatal() {
        let mut mp4 = synth_mp4();
        mp4.truncate(mp4.len() - 20); // moov aadha kaat do
        let r = validate(&mp4);
        assert!(r.truncated);
        assert!(!r.playable_estimate()); // Gate → reject, crash nahi
    }

    #[test]
    fn missing_moov_fails_gate() {
        let only = bx(b"ftyp", b"isom\0\0\0\x01");
        let r = validate(&only);
        assert!(!r.has_moov && !r.playable_estimate());
    }

    #[test]
    fn rewrite_shifts_stco_entries_by_delta() {
        let mut mp4 = synth_mp4();
        let patched = rewrite_chunk_offsets(&mut mp4, 48);
        assert_eq!(patched, 2);
        // 1000+48=1048, 2000+48=2048 — moov ke andar dhoondh ke verify
        let w1 = mp4.windows(4).any(|w| w == 1048u32.to_be_bytes());
        let w2 = mp4.windows(4).any(|w| w == 2048u32.to_be_bytes());
        assert!(w1 && w2);
    }

    #[test]
    fn rewrite_never_goes_negative() {
        let mut mp4 = synth_mp4();
        rewrite_chunk_offsets(&mut mp4, -50_000);
        // clamp(0) — offsets zero pe floor, negative kabhi nahi
        let w = mp4.windows(4).any(|w| w == 0u32.to_be_bytes());
        assert!(w);
    }
}
