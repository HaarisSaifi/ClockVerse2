//! Bounded, cancellable recovery from stable images and existing folders.
use crate::{ntfs, partition};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

const BLOCK: usize = 4 * 1024 * 1024;
const CANDIDATE_LIMIT: usize = 64 * 1024 * 1024;
const MAX_RESULTS: usize = 10_000;
const MAX_OUTPUT: u64 = 20 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryFile {
    pub id: String,
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub extension: String,
    pub origin: String,
    pub integrity: String,
    pub sha256: String,
    pub offset: u64,
}
#[derive(Debug, Default, Clone, Serialize)]
pub struct RecoveryReport {
    pub files: Vec<RecoveryFile>,
    pub warnings: Vec<String>,
    pub cancelled: bool,
    pub partial: bool,
    pub bytes_scanned: u64,
    pub total_bytes: u64,
    pub skipped: u64,
    pub output_dir: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub phase: String,
    pub bytes_scanned: u64,
    pub total_bytes: u64,
    pub found: usize,
}

pub fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "Cancelled");
    Ok(())
}
fn safe_name(name: &str) -> String {
    let value: String = name
        .chars()
        .take(140)
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    if value.trim_matches(['.', ' ']).is_empty() {
        "unnamed.bin".into()
    } else {
        value
    }
}

pub fn is_direct_drive(path: &Path) -> Option<char> {
    let s = path.to_string_lossy();
    if s.starts_with(r"\\.\") {
        return s
            .trim_start_matches(r"\\.\")
            .chars()
            .next()
            .filter(|c| c.is_ascii_alphabetic())
            .map(|c| c.to_ascii_uppercase());
    }
    let trimmed = s.trim().trim_end_matches('\\').trim_end_matches('/');
    if trimmed.len() == 2 && trimmed.ends_with(':') {
        let first = trimmed.chars().next()?;
        if first.is_ascii_alphabetic() {
            return Some(first.to_ascii_uppercase());
        }
    }
    None
}

fn open_source_device(path: &Path) -> Result<(File, u64)> {
    if let Some(letter) = is_direct_drive(path) {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let dev_path = format!(r"\\.\{letter}:");
            let file = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(3) // FILE_SHARE_READ | FILE_SHARE_WRITE
                .open(&dev_path)
                .context(format!(
                    "Could not open drive {dev_path}. Please run ClockVerse as Administrator for direct drive scanning."
                ))?;
            let root = format!("{letter}:\\");
            let total = crate::timesnap::available_space(Path::new(&root)).unwrap_or(0);
            Ok((file, total))
        }
        #[cfg(not(windows))]
        {
            let file = File::open(path)?;
            let len = file.metadata()?.len();
            Ok((file, len))
        }
    } else {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok((file, len))
    }
}

pub fn validate_destination(source: &Path, destination: &Path) -> Result<()> {
    let dest =
        fs::canonicalize(destination).context("Choose an existing recovery destination folder")?;
    ensure!(dest.is_dir(), "Destination must be a folder");

    if let Some(src_letter) = is_direct_drive(source) {
        let dest_str = dest.to_string_lossy().to_uppercase();
        let dest_letter = dest_str.chars().next().unwrap_or(' ');
        ensure!(
            src_letter != dest_letter,
            format!("Anti-Overwrite Protection: Cannot save recovered files onto drive {src_letter}: while scanning it! Please choose a DIFFERENT drive (e.g. external USB or secondary partition) so deleted data is not overwritten.")
        );
        return Ok(());
    }

    let src = fs::canonicalize(source).context("Source is unavailable")?;
    ensure!(
        !dest.starts_with(&src),
        "Recovery destination must be outside the source"
    );
    // Folder recovery writes must use another volume; image scans read an existing image file.
    if src.is_dir() {
        ensure!(
            volume_key(&src)? != volume_key(&dest)?,
            "Choose another volume for folder recovery so deleted data is not overwritten"
        );
    }
    Ok(())
}
#[cfg(windows)]
fn volume_key(path: &Path) -> Result<String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetVolumePathNameW(file: *const u16, volume: *mut u16, len: u32) -> i32;
        fn GetVolumeNameForVolumeMountPointW(root: *const u16, name: *mut u16, len: u32) -> i32;
    }
    let input: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut root = vec![0u16; 32768];
    let mut name = vec![0u16; 1024];
    ensure!(
        unsafe { GetVolumePathNameW(input.as_ptr(), root.as_mut_ptr(), root.len() as u32) } != 0,
        "Cannot identify source/destination volume"
    );
    ensure!(
        unsafe {
            GetVolumeNameForVolumeMountPointW(root.as_ptr(), name.as_mut_ptr(), name.len() as u32)
        } != 0,
        "Cannot identify source/destination volume"
    );
    Ok(
        String::from_utf16_lossy(&name[..name.iter().position(|c| *c == 0).unwrap_or(name.len())])
            .to_lowercase(),
    )
}
#[cfg(unix)]
fn volume_key(path: &Path) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    Ok(fs::metadata(path)?.dev().to_string())
}
#[cfg(not(any(windows, unix)))]
fn volume_key(_: &Path) -> Result<String> {
    anyhow::bail!("Volume identification unsupported")
}

fn write_candidate<R: Read>(
    mut reader: R,
    size: u64,
    name: &str,
    origin: &str,
    integrity: &str,
    offset: u64,
    out: &Path,
    cancel: &AtomicBool,
) -> Result<RecoveryFile> {
    let id = uuid::Uuid::new_v4().to_string();
    let dest = out.join(format!("{}_{}", &id[..8], safe_name(name)));
    let mut temp = tempfile::NamedTempFile::new_in(out)?;
    let mut buffer = vec![0; 256 * 1024];
    let mut remaining = size;
    let mut hash = Sha256::new();
    while remaining > 0 {
        check_cancel(cancel)?;
        let want = remaining.min(buffer.len() as u64) as usize;
        reader
            .read_exact(&mut buffer[..want])
            .context("Source ended before expected file length")?;
        temp.write_all(&buffer[..want])?;
        hash.update(&buffer[..want]);
        remaining -= want as u64;
    }
    temp.as_file().sync_all()?;
    temp.persist_noclobber(&dest)?;
    Ok(RecoveryFile {
        id,
        name: name.into(),
        path: dest.to_string_lossy().into(),
        size_bytes: size,
        extension: Path::new(name)
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase(),
        origin: origin.into(),
        integrity: integrity.into(),
        sha256: format!("{:x}", hash.finalize()),
        offset,
    })
}

/// Container-aware bounds. A complete boundary is not a guarantee of original content.
fn carve_length(bytes: &[u8], ext: &str) -> Option<(usize, &'static str)> {
    match ext {
        "jpg" => bytes
            .windows(2)
            .position(|w| w == b"\xff\xd9")
            .map(|n| (n + 2, "boundary found; preview required")),
        "png" => {
            let mut p = 8usize;
            let mut first = true;
            while p.checked_add(12)? <= bytes.len() {
                let size = u32::from_be_bytes(bytes[p..p + 4].try_into().ok()?) as usize;
                let end = p.checked_add(size)?.checked_add(12)?;
                if end > bytes.len() {
                    return None;
                }
                let kind = &bytes[p + 4..p + 8];
                if first && (kind != b"IHDR" || size != 13) {
                    return None;
                }
                first = false;
                if crc32(&bytes[p + 4..end - 4])
                    != u32::from_be_bytes(bytes[end - 4..end].try_into().ok()?)
                {
                    return None;
                }
                if kind == b"IEND" {
                    return (size == 0).then_some((end, "PNG chunk CRCs checked"));
                }
                p = end;
            }
            None
        }
        "pdf" => bytes
            .windows(5)
            .position(|w| w == b"%%EOF")
            .map(|n| (n + 5, "PDF boundary; latest revision not guaranteed")),
        "zip" => {
            for (p, _) in bytes
                .windows(4)
                .enumerate()
                .filter(|(_, w)| *w == b"PK\x05\x06")
            {
                if p + 22 > bytes.len() {
                    continue;
                }
                let comment = u16::from_le_bytes(bytes[p + 20..p + 22].try_into().ok()?) as usize;
                let end = p + 22 + comment;
                let cd_size = u32::from_le_bytes(bytes[p + 12..p + 16].try_into().ok()?) as usize;
                let cd_offset = u32::from_le_bytes(bytes[p + 16..p + 20].try_into().ok()?) as usize;
                if end <= bytes.len()
                    && cd_offset.checked_add(cd_size) == Some(p)
                    && bytes.get(cd_offset..cd_offset + 4) == Some(b"PK\x01\x02")
                {
                    return Some((end, "ZIP directory bounds; member CRCs not checked"));
                }
            }
            None
        }
        "mp4" => {
            let report = crate::mp4::validate(bytes);
            let mut end = 0;
            let mut media = false;
            let mut meta = false;
            for b in report.top_level_boxes {
                if !matches!(
                    b.typ.as_str(),
                    "ftyp"
                        | "mdat"
                        | "moov"
                        | "free"
                        | "skip"
                        | "wide"
                        | "uuid"
                        | "moof"
                        | "sidx"
                        | "styp"
                        | "mfra"
                ) {
                    break;
                }
                let next = usize::try_from(b.offset.checked_add(b.size)?).ok()?;
                if next > bytes.len() {
                    break;
                }
                media |= b.typ == "mdat";
                meta |= b.typ == "moov";
                end = next;
            }
            (media && meta && end > 0)
                .then_some((end, "MP4 boxes checked; playback not guaranteed"))
        }
        "riff" | "wav" | "avi" => {
            if bytes.len() >= 12 && &bytes[..4] == b"RIFF" {
                let size = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
                let end = size.checked_add(8)?;
                if end <= bytes.len() && end >= 12 {
                    let sub = &bytes[8..12];
                    if sub == b"WAVE" {
                        return Some((end, "RIFF WAVE audio container checked"));
                    } else if sub == b"AVI " {
                        return Some((end, "RIFF AVI video container checked"));
                    }
                }
            }
            None
        }
        "mp3" => {
            if bytes.len() >= 10 && &bytes[..3] == b"ID3" {
                let tag_size = ((bytes[6] as usize & 0x7F) << 21)
                    | ((bytes[7] as usize & 0x7F) << 14)
                    | ((bytes[8] as usize & 0x7F) << 7)
                    | (bytes[9] as usize & 0x7F);
                let header_len = 10 + tag_size;
                if header_len <= bytes.len() {
                    let max_mp3 = (15 * 1024 * 1024).min(bytes.len());
                    return Some((max_mp3, "ID3 audio container; playback depends on MPEG stream"));
                }
            }
            None
        }
        _ => None,
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for b in bytes {
        crc ^= *b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

fn volumes(file: &mut File) -> Result<Vec<(u64, ntfs::NtfsGeometry)>> {
    let mut sector = [0; 512];
    file.seek(SeekFrom::Start(0))?;
    if file.read_exact(&mut sector).is_err() {
        return Ok(vec![]);
    }
    if let Some(geo) = ntfs::parse_boot_sector(&sector) {
        return Ok(vec![(0, geo)]);
    }
    let mut parts = partition::parse_mbr(&sector);
    let mut header = [0; 512];
    file.read_exact(&mut header)?;
    if &header[..8] == b"EFI PART" {
        let lba = u64::from_le_bytes(header[72..80].try_into()?);
        let count = u32::from_le_bytes(header[80..84].try_into()?) as usize;
        let size = u32::from_le_bytes(header[84..88].try_into()?) as usize;
        if let Some(len) = count.checked_mul(size).filter(|n| *n <= 16 * 1024 * 1024) {
            let mut entries = vec![0; len];
            file.seek(SeekFrom::Start(
                lba.checked_mul(512).context("GPT overflow")?,
            ))?;
            file.read_exact(&mut entries)?;
            parts = partition::parse_gpt(&header, &entries);
        }
    }
    let mut result = vec![];
    for p in parts {
        let Some(base) = p.first_lba.checked_mul(512) else {
            continue;
        };
        file.seek(SeekFrom::Start(base))?;
        if file.read_exact(&mut sector).is_ok() {
            if let Some(g) = ntfs::parse_boot_sector(&sector) {
                result.push((base, g));
            }
        }
    }
    result.sort_by_key(|(base, _)| *base);
    Ok(result)
}

/// Stream unnamed NTFS data using volume-relative runs, including fragmented/sparse files.
fn recover_record(
    file: &mut File,
    base: u64,
    geo: &ntfs::NtfsGeometry,
    raw: &[u8],
    offset: u64,
    out: &Path,
    remaining_budget: u64,
    query: &str,
    cancel: &AtomicBool,
) -> Result<Option<RecoveryFile>> {
    let Some(rec) = ntfs::parse_record(raw, offset, geo.sector_size) else {
        return Ok(None);
    };
    if rec.in_use || rec.is_directory || !rec.fixup_ok {
        return Ok(None);
    }
    let Some(name) = rec
        .file_names
        .iter()
        .find(|n| matches_query(&n.name, query))
    else {
        return Ok(None);
    };
    let mut fixed = raw.to_vec();
    ensure!(
        ntfs::apply_fixup(&mut fixed, geo.sector_size),
        "MFT fixup failed"
    );
    let mut p = u16::from_le_bytes(fixed[20..22].try_into()?) as usize;
    while p + 24 <= fixed.len() {
        let kind = u32::from_le_bytes(fixed[p..p + 4].try_into()?);
        let len = u32::from_le_bytes(fixed[p + 4..p + 8].try_into()?) as usize;
        if kind == ntfs::ATTR_END || len < 24 || p + len > fixed.len() {
            break;
        }
        if kind == ntfs::ATTR_DATA && fixed[p + 9] == 0 {
            let flags = u16::from_le_bytes(fixed[p + 12..p + 14].try_into()?);
            ensure!(
                flags & 0x4001 == 0,
                "Compressed/encrypted NTFS stream is unsupported"
            );
            if fixed[p + 8] == 0 {
                let size = u32::from_le_bytes(fixed[p + 16..p + 20].try_into()?) as usize;
                let start = u16::from_le_bytes(fixed[p + 20..p + 22].try_into()?) as usize;
                ensure!(
                    start >= 24 && start + size <= len && size as u64 <= remaining_budget,
                    "Invalid or over-budget resident data"
                );
                return Ok(Some(write_candidate(
                    &fixed[p + start..p + start + size],
                    size as u64,
                    &name.name,
                    "NTFS deleted resident",
                    "MFT fixup checked; content hash recorded",
                    offset,
                    out,
                    cancel,
                )?));
            }
            ensure!(
                len >= 64 && u64::from_le_bytes(fixed[p + 16..p + 24].try_into()?) == 0,
                "NTFS extension record unsupported"
            );
            let size = u64::from_le_bytes(fixed[p + 48..p + 56].try_into()?);
            let initialized = u64::from_le_bytes(fixed[p + 56..p + 64].try_into()?);
            ensure!(
                size <= remaining_budget && initialized <= size,
                "NTFS file exceeds output budget or invalid initialized size"
            );
            let mut temp = tempfile::NamedTempFile::new_in(out)?;
            let mut buffer = vec![0; 256 * 1024];
            let mut written = 0u64;
            let mut hasher = Sha256::new();
            for run in &rec.data_runs {
                check_cancel(cancel)?;
                if written >= initialized {
                    break;
                }
                let mut left = run
                    .length
                    .checked_mul(geo.cluster_size)
                    .context("Run overflow")?
                    .min(initialized - written);
                if run.lcn >= 0 {
                    file.seek(SeekFrom::Start(
                        base.checked_add(
                            (run.lcn as u64)
                                .checked_mul(geo.cluster_size)
                                .context("LCN overflow")?,
                        )
                        .context("Volume overflow")?,
                    ))?;
                } else {
                    ensure!(run.lcn == -1, "Invalid negative LCN");
                }
                while left > 0 {
                    check_cancel(cancel)?;
                    let n = left.min(buffer.len() as u64) as usize;
                    if run.lcn == -1 {
                        buffer[..n].fill(0);
                    } else {
                        file.read_exact(&mut buffer[..n])
                            .context("Unreadable or truncated NTFS run")?;
                    }
                    temp.write_all(&buffer[..n])?;
                    hasher.update(&buffer[..n]);
                    written += n as u64;
                    left -= n as u64;
                }
            }
            ensure!(
                written == initialized,
                "Missing NTFS extents; partial file not published"
            );
            buffer.fill(0);
            while written < size {
                check_cancel(cancel)?;
                let n = (size - written).min(buffer.len() as u64) as usize;
                temp.write_all(&buffer[..n])?;
                hasher.update(&buffer[..n]);
                written += n as u64;
            }
            temp.as_file().sync_all()?;
            let id = uuid::Uuid::new_v4().to_string();
            let dest = out.join(format!("{}_{}", &id[..8], safe_name(&name.name)));
            temp.persist_noclobber(&dest)?;
            return Ok(Some(RecoveryFile {
                id,
                name: name.name.clone(),
                path: dest.to_string_lossy().into(),
                size_bytes: size,
                extension: Path::new(&name.name)
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase(),
                origin: "NTFS deleted extents".into(),
                integrity: "Extent lengths checked; overwrite cannot be ruled out".into(),
                sha256: format!("{:x}", hasher.finalize()),
                offset,
            }));
        }
        p += len;
    }
    Ok(None)
}

/// Extract a quoted filename or filename token from a short natural-language request.
pub fn search_term(request: &str) -> String {
    for quote in ['"', '\''] {
        let parts: Vec<&str> = request.split(quote).collect();
        if parts.len() >= 3 && !parts[1].trim().is_empty() {
            return parts[1].trim().into();
        }
    }
    request
        .split_whitespace()
        .find(|word| {
            let word = word.trim_matches([',', '?', '!']);
            word.contains('.') && word.bytes().any(|b| b.is_ascii_alphabetic())
        })
        .map(|s| s.trim_matches([',', '?', '!']).to_string())
        .unwrap_or_else(|| request.trim().to_string())
}

pub fn matches_query(name: &str, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty() || name.to_lowercase().contains(&q)
}
pub fn run(
    source: &Path,
    destination: &Path,
    query: &str,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<RecoveryReport> {
    validate_destination(source, destination)?;
    let out = destination.join(format!("ClockVerse_{}", uuid::Uuid::new_v4()));
    fs::create_dir(&out)?;
    let mut report = RecoveryReport {
        output_dir: out.to_string_lossy().into(),
        ..Default::default()
    };
    let mut output_bytes = 0u64;
    let scan_result: Result<()> = (|| {
        let is_drive = is_direct_drive(source);
        let is_quick = query.starts_with(":quick:");
        let clean_query = query.trim_start_matches(":quick:").trim();

        if is_drive.is_none() && source.is_dir() {
            report.warnings.push("Folder mode copies existing files only. For deleted files without snapshots, select a drive or disk image. Links are skipped.".into());
            for entry in walkdir::WalkDir::new(source).follow_links(false) {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => {
                        report.skipped += 1;
                        continue;
                    }
                };
                if !entry.file_type().is_file()
                    || !matches_query(&entry.file_name().to_string_lossy(), clean_query)
                {
                    continue;
                }
                let size = entry.metadata()?.len();
                if output_bytes.saturating_add(size) > MAX_OUTPUT
                    || report.files.len() >= MAX_RESULTS
                {
                    report.partial = true;
                    report
                        .warnings
                        .push("Output budget reached: 20 GiB / 10,000 files.".into());
                    break;
                }
                match write_candidate(
                    File::open(entry.path())?,
                    size,
                    &entry.file_name().to_string_lossy(),
                    "Existing file",
                    "Copy hash recorded; not deleted-file recovery",
                    0,
                    &out,
                    cancel,
                ) {
                    Ok(f) => {
                        output_bytes += size;
                        report.files.push(f);
                    }
                    Err(e) => {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        return Err(e);
                    }
                }
                progress(Progress {
                    phase: "Copying existing files".into(),
                    bytes_scanned: output_bytes,
                    total_bytes: 0,
                    found: report.files.len(),
                });
            }
        } else {
            let (mut input, total_len) = open_source_device(source)?;
            let initial_len = total_len;
            let initial_modified = input.metadata().ok().and_then(|m| m.modified().ok());
            report.total_bytes = total_len;
            let (mut extract, _) = open_source_device(source)?;
            let vols = match volumes(&mut extract) {
                Ok(v) => v,
                Err(e) => {
                    report.warnings.push(format!(
                        "Partition metadata unavailable: {e}. Signature scan continues."
                    ));
                    vec![]
                }
            };
            if is_quick && !vols.is_empty() {
                report.warnings.push("Quick MFT Scan: rapidly searching Master File Table for deleted records.".into());
                for (base, geo) in &vols {
                    let mft_byte_offset = base + geo.mft_lcn * geo.cluster_size;
                    let mut rec_buf = vec![0u8; geo.record_size];
                    let max_scan_records = 100_000u64;
                    let _ = extract.seek(SeekFrom::Start(mft_byte_offset));
                    for rec_num in 0..max_scan_records {
                        if cancel.load(Ordering::Relaxed)
                            || report.files.len() >= MAX_RESULTS
                            || output_bytes >= MAX_OUTPUT
                        {
                            break;
                        }
                        if extract.read_exact(&mut rec_buf).is_err() {
                            break;
                        }
                        if &rec_buf[..4] == b"FILE" {
                            let absolute = mft_byte_offset + rec_num * geo.record_size as u64;
                            if let Ok(Some(f)) = recover_record(
                                &mut extract,
                                *base,
                                geo,
                                &rec_buf,
                                absolute,
                                &out,
                                MAX_OUTPUT - output_bytes,
                                clean_query,
                                cancel,
                            ) {
                                output_bytes += f.size_bytes;
                                report.files.push(f);
                            }
                        }
                    }
                }
                report.bytes_scanned = report.total_bytes;
                progress(Progress {
                    phase: "Quick MFT scan finished".into(),
                    bytes_scanned: report.total_bytes,
                    total_bytes: report.total_bytes,
                    found: report.files.len(),
                });
            } else {
                report.warnings.push("Carved names may be lost. Signature candidates are limited to 64 MiB; output budget is 20 GiB / 10,000 files. No deletion-date cutoff is applied.".into());
                let patterns: Vec<&[u8]> = vec![
                    b"\xff\xd8\xff",
                    b"\x89PNG\r\n\x1a\n",
                    b"%PDF-",
                    b"PK\x03\x04",
                    b"ftyp",
                    b"RIFF",
                    b"ID3",
                    b"FILE",
                ];
                let extensions = ["jpg", "png", "pdf", "zip", "mp4", "riff", "mp3", "mft"];
                let ac = aho_corasick::AhoCorasick::new(patterns)?;
                let mut extra_reads = 0u64;
                let mut read_budget_warned = false;
                let mut block = vec![0; BLOCK + 65536];
                let mut offset = 0u64;
                let mut last_emit = std::time::Instant::now();
                let scan_limit = if is_quick {
                    (1024 * 1024 * 1024u64).min(report.total_bytes)
                } else {
                    report.total_bytes
                };
                while offset < scan_limit {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    if report.files.len() >= MAX_RESULTS || output_bytes >= MAX_OUTPUT {
                        report
                            .warnings
                            .push("Output budget reached; scan is partial.".into());
                        break;
                    }
                    let len = (scan_limit - offset).min(block.len() as u64) as usize;
                    if input.seek(SeekFrom::Start(offset)).is_err() {
                        offset = offset.saturating_add(65536).min(scan_limit);
                        continue;
                    }
                    if let Err(e) = input.read_exact(&mut block[..len]) {
                        if report.warnings.len() < 10 {
                            report.warnings.push(format!("Damaged sector at offset {offset}: {e}. Skipping block to preserve recovery integrity."));
                        }
                        offset = offset.saturating_add(65536).min(scan_limit);
                        continue;
                    }
                    for hit in ac.find_iter(&block[..len]) {
                        if hit.start() >= BLOCK {
                            break;
                        }
                        if cancel.load(Ordering::Relaxed)
                            || report.files.len() >= MAX_RESULTS
                            || output_bytes >= MAX_OUTPUT
                        {
                            break;
                        }
                        let ext = extensions[hit.pattern().as_usize()];
                        let absolute = offset + hit.start() as u64;
                        if ext == "mft" {
                            if absolute % 512 != 0 {
                                continue;
                            }
                            let Some((base, geo)) =
                                vols.iter().rev().find(|(base, _)| *base <= absolute)
                            else {
                                continue;
                            };
                            let end = hit.start() + geo.record_size;
                            if end > len {
                                continue;
                            }
                            match recover_record(
                                &mut extract,
                                *base,
                                geo,
                                &block[hit.start()..end],
                                absolute,
                                &out,
                                MAX_OUTPUT - output_bytes,
                                clean_query,
                                cancel,
                            ) {
                                Ok(Some(f)) => {
                                    output_bytes += f.size_bytes;
                                    report.files.push(f);
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    report.skipped += 1;
                                    if report.warnings.len() < 12 {
                                        report
                                            .warnings
                                            .push(format!("NTFS candidate at {absolute}: {e}"));
                                    }
                                }
                            }
                        } else {
                            let filter_ext = Path::new(clean_query)
                                .extension()
                                .and_then(|e| e.to_str())
                                .unwrap_or(clean_query.trim_start_matches('.'));
                            if !clean_query.is_empty()
                                && !filter_ext.eq_ignore_ascii_case(ext)
                                && !(ext == "jpg" && filter_ext.eq_ignore_ascii_case("jpeg"))
                                && !(ext == "zip" && matches!(filter_ext.to_ascii_lowercase().as_str(), "docx" | "xlsx" | "pptx"))
                                && !(ext == "riff" && matches!(filter_ext.to_ascii_lowercase().as_str(), "wav" | "avi"))
                            {
                                continue;
                            }
                            let start = if ext == "mp4" {
                                match absolute.checked_sub(4) {
                                    Some(v) => v,
                                    None => continue,
                                }
                            } else {
                                absolute
                            };
                            let cap = (report.total_bytes - start)
                                .min(CANDIDATE_LIMIT as u64)
                                .min(MAX_OUTPUT - output_bytes)
                                as usize;
                            let mut candidate = Vec::new();
                            let mut boundary = None;
                            if extract.seek(SeekFrom::Start(start)).is_err() {
                                continue;
                            }
                            while candidate.len() < cap {
                                check_cancel(cancel)?;
                                let next = if candidate.is_empty() {
                                    256 * 1024
                                } else {
                                    candidate.len().saturating_mul(2)
                                }
                                .min(cap);
                                let additional = next - candidate.len();
                                if extra_reads.saturating_add(additional as u64)
                                    > 8 * 1024 * 1024 * 1024
                                {
                                    report.partial = true;
                                    if !read_budget_warned {
                                        report.warnings.push("Signature validation read budget reached (8 GiB). Remaining signature candidates skipped; NTFS scan continues. Try a targeted file extension scan.".into());
                                        read_budget_warned = true;
                                    }
                                    break;
                                }
                                let previous = candidate.len();
                                candidate.resize(next, 0);
                                if let Err(e) = extract.read_exact(&mut candidate[previous..]) {
                                    if report.warnings.len() < 10 {
                                        report.warnings.push(format!("Damaged extent at {start}: {e}"));
                                    }
                                    break;
                                }
                                extra_reads += additional as u64;
                                boundary = carve_length(&candidate, ext);
                                if boundary.is_some() {
                                    break;
                                }
                            }
                            if let Some((size, integrity)) = boundary {
                                let (final_ext, origin_desc) = if ext == "zip" {
                                    let sample_len = size.min(128 * 1024);
                                    let sample = &candidate[..sample_len];
                                    if sample.windows(5).any(|w| w == b"word/") {
                                        ("docx", "Carved Word Document")
                                    } else if sample.windows(3).any(|w| w == b"xl/") {
                                        ("xlsx", "Carved Excel Spreadsheet")
                                    } else if sample.windows(4).any(|w| w == b"ppt/") {
                                        ("pptx", "Carved PowerPoint")
                                    } else {
                                        ("zip", "Carved ZIP Archive")
                                    }
                                } else if ext == "riff" {
                                    if size >= 12 && &candidate[8..12] == b"WAVE" {
                                        ("wav", "Carved WAV Audio")
                                    } else if size >= 12 && &candidate[8..12] == b"AVI " {
                                        ("avi", "Carved AVI Video")
                                    } else {
                                        ("riff", "Carved RIFF Media")
                                    }
                                } else if ext == "mp3" {
                                    ("mp3", "Carved MP3 Audio")
                                } else {
                                    (ext, "Signature candidate")
                                };
                                let name = format!("candidate_{start}.{final_ext}");
                                match write_candidate(
                                    &candidate[..size],
                                    size as u64,
                                    &name,
                                    origin_desc,
                                    integrity,
                                    start,
                                    &out,
                                    cancel,
                                ) {
                                    Ok(f) => {
                                        output_bytes += f.size_bytes;
                                        report.files.push(f);
                                    }
                                    Err(e) => {
                                        if cancel.load(Ordering::Relaxed) {
                                            break;
                                        }
                                        return Err(e);
                                    }
                                }
                            } else {
                                report.skipped += 1;
                            }
                        }
                    }
                    offset = offset.saturating_add(BLOCK as u64).min(scan_limit);
                    report.bytes_scanned = offset;
                    if last_emit.elapsed().as_millis() >= 150 || offset == scan_limit {
                        progress(Progress {
                            phase: "NTFS records + signature sweep".into(),
                            bytes_scanned: offset,
                            total_bytes: scan_limit,
                            found: report.files.len(),
                        });
                        last_emit = std::time::Instant::now();
                    }
                }
                report.bytes_scanned = report.total_bytes;
                if is_drive.is_none() {
                    if let Ok(after) = input.metadata() {
                        if initial_len != after.len() || initial_modified != after.modified().ok() {
                            report.partial = true;
                            report.warnings.push("Source image changed during scan. Results may be inconsistent; rescan a stable copy.".into());
                        }
                    }
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = scan_result {
        report.partial = true;
        report.warnings.push(format!(
            "Scan stopped early: {error}. Completed files are preserved."
        ));
    }
    if report.files.len() >= MAX_RESULTS || output_bytes >= MAX_OUTPUT {
        report.partial = true;
        report
            .warnings
            .push("Session output limit reached; additional candidates may remain.".into());
    }
    report.cancelled = cancel.load(Ordering::Relaxed);
    report.partial |=
        report.cancelled || (report.total_bytes > 0 && report.bytes_scanned < report.total_bytes);
    let mut manifest = tempfile::NamedTempFile::new_in(&out)?;
    serde_json::to_writer_pretty(&mut manifest, &report)?;
    manifest.as_file().sync_all()?;
    manifest.persist_noclobber(out.join("recovery-report.json"))?;
    Ok(report)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn deleted_record(resident: bool) -> Vec<u8> {
        let mut raw = vec![0u8; 1024];
        raw[..4].copy_from_slice(b"FILE");
        raw[4..6].copy_from_slice(&48u16.to_le_bytes());
        raw[6..8].copy_from_slice(&3u16.to_le_bytes());
        raw[20..22].copy_from_slice(&56u16.to_le_bytes());
        let name: Vec<u16> = "invoice.txt".encode_utf16().collect();
        let attr_len = 24 + 66 + name.len() * 2;
        raw[56..60].copy_from_slice(&ntfs::ATTR_FILE_NAME.to_le_bytes());
        raw[60..64].copy_from_slice(&(attr_len as u32).to_le_bytes());
        raw[72..76].copy_from_slice(&((attr_len - 24) as u32).to_le_bytes());
        raw[76..78].copy_from_slice(&24u16.to_le_bytes());
        raw[80 + 64] = name.len() as u8;
        for (i, c) in name.iter().enumerate() {
            raw[80 + 66 + i * 2..80 + 68 + i * 2].copy_from_slice(&c.to_le_bytes());
        }
        let p = 56 + attr_len;
        raw[p..p + 4].copy_from_slice(&ntfs::ATTR_DATA.to_le_bytes());
        if resident {
            raw[p + 4..p + 8].copy_from_slice(&32u32.to_le_bytes());
            raw[p + 16..p + 20].copy_from_slice(&8u32.to_le_bytes());
            raw[p + 20..p + 22].copy_from_slice(&24u16.to_le_bytes());
            raw[p + 24..p + 32].copy_from_slice(b"original");
        } else {
            raw[p + 4..p + 8].copy_from_slice(&80u32.to_le_bytes());
            raw[p + 8] = 1;
            raw[p + 32..p + 34].copy_from_slice(&64u16.to_le_bytes());
            raw[p + 48..p + 56].copy_from_slice(&1030u64.to_le_bytes());
            raw[p + 56..p + 64].copy_from_slice(&1030u64.to_le_bytes());
            raw[p + 64..p + 73].copy_from_slice(&[0x11, 1, 20, 0x11, 1, 20, 0x01, 1, 0]);
        }
        raw[48..50].copy_from_slice(&[0xaa, 0xbb]);
        for i in 0..2 {
            let p = (i + 1) * 512 - 2;
            let saved = [raw[p], raw[p + 1]];
            raw[50 + i * 2..52 + i * 2].copy_from_slice(&saved);
            raw[p..p + 2].copy_from_slice(&[0xaa, 0xbb]);
        }
        raw
    }
    fn ntfs_image(resident: bool) -> Vec<u8> {
        let base = 4096;
        let mut image = vec![0; 65536];
        image[510..512].copy_from_slice(b"\x55\xaa");
        image[450] = 7;
        image[454..458].copy_from_slice(&8u32.to_le_bytes());
        image[458..462].copy_from_slice(&120u32.to_le_bytes());
        image[base + 3..base + 11].copy_from_slice(b"NTFS    ");
        image[base + 11..base + 13].copy_from_slice(&512u16.to_le_bytes());
        image[base + 13] = 1;
        image[base + 0x40] = 0xf6;
        image[base + 512..base + 1536].copy_from_slice(&deleted_record(resident));
        image[base + 20 * 512..base + 21 * 512].fill(0x41);
        image[base + 40 * 512..base + 41 * 512].fill(0x42);
        image
    }
    #[test]
    fn conversational_filename_and_extension_queries() {
        assert_eq!(search_term("meri invoice.pdf nahi aayi"), "invoice.pdf");
        assert_eq!(
            search_term("find \"Annual report.pdf\" please"),
            "Annual report.pdf"
        );
        assert_eq!(search_term(".jpg"), ".jpg");
    }
    #[test]
    fn recovers_deleted_resident_without_snapshot_and_uses_data_attribute_size() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        fs::write(&image, ntfs_image(true)).unwrap();
        let report = run(
            &image,
            tmp.path(),
            "invoice.txt",
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(report.files.len(), 1);
        assert_eq!(fs::read(&report.files[0].path).unwrap(), b"original");
        assert!(report.files[0].origin.contains("deleted"));
        assert!(Path::new(&report.output_dir)
            .join("recovery-report.json")
            .exists());
    }
    #[test]
    fn recovers_fragmented_partition_relative_runs_and_sparse_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        fs::write(&image, ntfs_image(false)).unwrap();
        let report = run(
            &image,
            tmp.path(),
            "invoice.txt",
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(report.files.len(), 1);
        let data = fs::read(&report.files[0].path).unwrap();
        assert_eq!(data.len(), 1030);
        assert!(data[..512].iter().all(|b| *b == 0x41));
        assert!(data[512..1024].iter().all(|b| *b == 0x42));
        assert_eq!(&data[1024..], &[0; 6]);
    }
    #[test]
    fn cancellation_and_chunk_boundary_are_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let mut data = vec![0; BLOCK + 100];
        data[BLOCK - 2..BLOCK + 5].copy_from_slice(b"%PDF-12");
        data[BLOCK + 20..BLOCK + 25].copy_from_slice(b"%%EOF");
        fs::write(&image, data).unwrap();
        let cancelled = run(&image, tmp.path(), "", &AtomicBool::new(true), |_| {}).unwrap();
        assert!(cancelled.cancelled);
        assert!(cancelled.files.is_empty());
        let report = run(&image, tmp.path(), ".pdf", &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].offset, BLOCK as u64 - 2);
    }
    #[test]
    fn corrupt_fixup_and_truncated_extents_are_not_published() {
        let tmp = tempfile::tempdir().unwrap();
        let image = tmp.path().join("disk.img");
        let mut data = ntfs_image(true);
        data[4096 + 512 + 510] = 0;
        fs::write(&image, &data).unwrap();
        assert!(run(
            &image,
            tmp.path(),
            "invoice.txt",
            &AtomicBool::new(false),
            |_| {}
        )
        .unwrap()
        .files
        .is_empty());
        let mut data = ntfs_image(false);
        data.truncate(4096 + 25 * 512);
        fs::write(&image, &data).unwrap();
        let report = run(
            &image,
            tmp.path(),
            "invoice.txt",
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert!(report.files.is_empty());
        assert_eq!(report.skipped, 1);
    }
    #[test]
    fn png_crc_gate_and_malformed_zip_are_safe() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, body) in [(b"IHDR", vec![0u8; 13]), (b"IEND", vec![])] {
            png.extend_from_slice(&(body.len() as u32).to_be_bytes());
            let start = png.len();
            png.extend_from_slice(kind);
            png.extend(body);
            let crc = crc32(&png[start..]);
            png.extend_from_slice(&crc.to_be_bytes());
        }
        assert_eq!(carve_length(&png, "png").unwrap().0, png.len());
        png[20] ^= 1;
        assert!(carve_length(&png, "png").is_none());
        assert!(carve_length(b"PK\x03\x04junkPK\x05\x06", "zip").is_none());
    }
    #[test]
    fn invalid_geometry_and_run_headers_never_panic() {
        let mut bs = vec![0; 512];
        bs[3..11].copy_from_slice(b"NTFS    ");
        bs[11..13].copy_from_slice(&512u16.to_le_bytes());
        bs[13] = 1;
        bs[0x40] = 128;
        assert!(ntfs::parse_boot_sector(&bs).is_none());
        assert!(ntfs::parse_data_runs(&[0xff; 32]).is_empty());
        assert!(!ntfs::apply_fixup(&mut vec![0; 1024], 0));
        assert_eq!(ntfs::parse_data_runs(&[0x01, 2, 0])[0].lcn, -1);
    }
}
