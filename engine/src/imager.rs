//! Forensic imager: source (disk/file) → .dd with checkpoint resume.
//! 500 GB overnight job ka gate: power cut / crash ke baad EXACT byte se resume,
//! restart from zero kabhi nahi. .ckpt sidecar file = crash-safe progress.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};

#[derive(Debug, Clone, Copy)]
pub struct ImagingProgress {
    pub bytes_done: u64,
    pub bytes_total: u64,
}

/// Image a source to .dd. `limit` sirf testing ke liye (partial pass simulate).
///
/// Invariants:
/// - Source READ-ONLY khulta hai — imager ke paas write capability hi nahi hoti.
/// - Checkpoint har chunk ke baad flush hota hai (crash point = last chunk).
/// - Complete hone pe .ckpt delete — ckpt file ka hona = image incomplete.
pub fn image_with_resume<F: FnMut(ImagingProgress)>(
    src: &str,
    dst: &str,
    chunk: usize,
    limit: Option<u64>,
    mut progress: F,
) -> anyhow::Result<u64> {
    // Source: read-only. Yeh line forensic safety ka hissa hai, convenience nahi.
    let mut reader = File::open(src)?;
    let total = reader.metadata()?.len();
    let ckpt_path = format!("{dst}.ckpt");

    let mut done: u64 = match std::fs::read_to_string(&ckpt_path) {
        Ok(s) => s.trim().parse().unwrap_or(0),
        Err(_) => 0,
    };
    if done > total {
        done = 0; // corrupt checkpoint — restart, kabhi trust nahi
    }

    reader.seek(SeekFrom::Start(done))?;
    let mut writer = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false) // checkpoint resume — must never truncate
        .open(dst)?;
    writer.seek(SeekFrom::Start(done))?;

    let mut buf = vec![0u8; chunk.max(1 << 16)];
    loop {
        if let Some(lim) = limit {
            if done >= lim {
                break;
            }
        }
        let want = limit
            .map(|l| ((l - done) as usize).min(buf.len()))
            .unwrap_or(buf.len());
        let n = reader.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        done += n as u64;
        // Checkpoint har chunk pe — 64 MB chunks pe overhead negligible
        std::fs::write(&ckpt_path, done.to_string())?;
        progress(ImagingProgress {
            bytes_done: done,
            bytes_total: total,
        });
    }
    writer.flush()?;
    if done >= total {
        std::fs::remove_file(&ckpt_path).ok(); // complete = no checkpoint
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imaging_resumes_from_checkpoint_byte_exact() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("disk.img");
        let data: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        let dst = dir.path().join("out.dd");
        let (s, d) = (
            src.to_str().unwrap().to_string(),
            dst.to_str().unwrap().to_string(),
        );

        // Pass 1: crash at 400k simulate
        let done1 = image_with_resume(&s, &d, 65_536, Some(400_000), |_| {}).unwrap();
        assert_eq!(done1, 400_000);
        assert!(std::path::Path::new(&format!("{d}.ckpt")).exists());

        // Pass 2: resume → byte-exact complete image
        let done2 = image_with_resume(&s, &d, 65_536, None, |_| {}).unwrap();
        assert_eq!(done2, 1_000_000);
        assert_eq!(std::fs::read(&d).unwrap(), data);
        assert!(!std::path::Path::new(&format!("{d}.ckpt")).exists());
    }
}
