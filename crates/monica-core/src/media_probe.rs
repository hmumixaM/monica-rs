use anyhow::{Context, Result, bail, ensure};
use image::ImageDecoder;
use serde::{Deserialize, Serialize};
use std::{
    io::{BufReader, Read},
    path::Path,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Inspection {
    pub media_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
}

/// Runs only in an isolated helper process, never on an async request thread.
pub fn inspect(path: &Path, name: &str) -> Result<Inspection> {
    let mut file = std::fs::File::open(path)?;
    let mut head = [0u8; 16384];
    let n = file.read(&mut head)?;
    let guessed = image::guess_format(&head[..n]).ok();
    let extension = Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let claimed_image = matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp");
    if let Some(format) = guessed {
        ensure!(
            matches!(
                format,
                image::ImageFormat::Jpeg
                    | image::ImageFormat::Png
                    | image::ImageFormat::Gif
                    | image::ImageFormat::WebP
            ),
            "unsupported image format"
        );
        let mut reader =
            image::ImageReader::with_format(BufReader::new(std::fs::File::open(path)?), format);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(16384);
        limits.max_image_height = Some(16384);
        limits.max_alloc = Some(96 * 1024 * 1024);
        reader.limits(limits);
        let decoded = reader.decode().context("invalid or oversized image")?;
        ensure!(
            u64::from(decoded.width()) * u64::from(decoded.height()) <= 24_000_000,
            "image exceeds pixel limit"
        );
        return Ok(Inspection {
            media_type: format.to_mime_type().into(),
            width: Some(decoded.width()),
            height: Some(decoded.height()),
            duration_ms: None,
        });
    }
    ensure!(
        !claimed_image,
        "file is named as an image but is not a valid image"
    );
    let video = if n >= 12 && &head[4..8] == b"ftyp" {
        ensure!(
            !matches!(&head[8..12], b"heic" | b"heix" | b"mif1" | b"avif"),
            "unsupported still-image codec"
        );
        Some(if &head[8..12] == b"qt  " {
            "video/quicktime"
        } else {
            "video/mp4"
        })
    } else if n >= 4 && head[..4] == [0x1a, 0x45, 0xdf, 0xa3] {
        Some("video/webm")
    } else {
        None
    };
    if let Some(kind) = video {
        // The enclosing process group has a hard wall-clock and CPU deadline.
        let output = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height,codec_type:format=duration",
                "-of",
                "json",
            ])
            .arg(path)
            .output()
            .context("video inspection requires bundled ffprobe")?;
        ensure!(
            output.status.success() && output.stdout.len() <= 16384,
            "invalid video"
        );
        let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let stream = parsed["streams"]
            .as_array()
            .and_then(|s| s.first())
            .context("video has no video stream")?;
        let width = stream["width"].as_u64().context("invalid video width")?;
        let height = stream["height"].as_u64().context("invalid video height")?;
        ensure!(
            width > 0 && height > 0 && width <= 16384 && height <= 16384,
            "video dimensions invalid"
        );
        return Ok(Inspection {
            media_type: kind.into(),
            width: Some(width as u32),
            height: Some(height as u32),
            duration_ms: parsed["format"]["duration"]
                .as_str()
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|seconds| {
                    seconds.is_finite() && *seconds >= 0.0 && *seconds <= 31_536_000.0
                })
                .map(|seconds| (seconds * 1000.0).round() as u64),
        });
    }
    if matches!(extension.as_str(), "mp4" | "mov" | "webm") {
        bail!("file is named as a video but is not a valid video")
    }
    let kind = infer::get(&head[..n])
        .map(|x| x.mime_type())
        .unwrap_or(match extension.as_str() {
            "txt" | "md" | "csv" => "text/plain",
            "html" | "htm" => "text/html",
            "svg" => "image/svg+xml",
            "json" => "application/json",
            _ => "application/octet-stream",
        });
    Ok(Inspection {
        media_type: kind.into(),
        width: None,
        height: None,
        duration_ms: None,
    })
}

fn preview(path: &Path, output: &Path, crop: Option<Vec<f32>>) -> Result<()> {
    ensure!(
        std::fs::metadata(path)?.len() <= 40 * 1024 * 1024,
        "preview source exceeds size limit"
    );
    let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(96 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    ensure!(
        u64::from(width) * u64::from(height) <= 24_000_000,
        "preview exceeds pixel limit"
    );
    let orientation = decoder.orientation()?;
    let mut decoded = image::DynamicImage::from_decoder(decoder)?;
    decoded.apply_orientation(orientation);
    let result = if let Some(crop) = crop {
        ensure!(
            crop.len() == 4
                && crop
                    .iter()
                    .all(|v| v.is_finite() && (-1.0..=2.0).contains(v))
                && crop[2] > 0.0
                && crop[3] > 0.0,
            "invalid face crop"
        );
        let w = decoded.width() as f32;
        let h = decoded.height() as f32;
        let edge = (crop[2] * w).max(crop[3] * h) * 1.5;
        let x = ((crop[0] + crop[2] / 2.0) * w - edge / 2.0)
            .max(0.0)
            .min(w - 1.0) as u32;
        let y = ((crop[1] + crop[3] / 2.0) * h - edge / 2.0)
            .max(0.0)
            .min(h - 1.0) as u32;
        let cw = (edge.ceil() as u32).max(1).min(decoded.width() - x);
        let ch = (edge.ceil() as u32).max(1).min(decoded.height() - y);
        let resized = decoded
            .crop_imm(x, y, cw, ch)
            .thumbnail(192, 192)
            .to_rgba8();
        let mut padded = image::RgbaImage::from_pixel(192, 192, image::Rgba([245, 245, 245, 255]));
        image::imageops::overlay(
            &mut padded,
            &resized,
            ((192 - resized.width()) / 2).into(),
            ((192 - resized.height()) / 2).into(),
        );
        image::DynamicImage::ImageRgba8(padded)
    } else {
        decoded.thumbnail(480, 480)
    };
    result.save_with_format(output, image::ImageFormat::WebP)?;
    std::fs::File::open(output)?.sync_all()?;
    Ok(())
}
pub fn entry() -> Result<bool> {
    let args = std::env::args_os().collect::<Vec<_>>();
    let is_preview = args.get(1).is_some_and(|x| x == "--monica-media-preview");
    if !is_preview && args.get(1).is_none_or(|x| x != "--monica-media-probe") {
        return Ok(false);
    }
    ensure!(
        args.len() == if is_preview { 5 } else { 4 },
        "invalid media probe arguments"
    );
    #[cfg(unix)]
    unsafe {
        let cpu = libc::rlimit {
            rlim_cur: 5,
            rlim_max: 6,
        };
        let memory = libc::rlimit {
            rlim_cur: 512 * 1024 * 1024,
            rlim_max: 512 * 1024 * 1024,
        };
        ensure!(
            libc::setrlimit(libc::RLIMIT_CPU, &cpu) == 0,
            "cannot bound media CPU"
        );
        // macOS address-space accounting includes mapped framework regions;
        // decoder allocation limits apply there, RLIMIT_AS applies on Linux.
        #[cfg(target_os = "linux")]
        ensure!(
            libc::setrlimit(libc::RLIMIT_AS, &memory) == 0,
            "cannot bound media memory"
        );
        #[cfg(not(target_os = "linux"))]
        let _ = memory;
    }
    if is_preview {
        preview(
            Path::new(&args[2]),
            Path::new(&args[3]),
            serde_json::from_str(&args[4].to_string_lossy())?,
        )?;
        println!("{{}}");
    } else {
        let inspected = inspect(Path::new(&args[2]), &args[3].to_string_lossy())?;
        println!("{}", serde_json::to_string(&inspected)?);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_validation_and_previews_use_real_bounded_decoder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("synthetic.png");
        image::RgbImage::from_pixel(640, 320, image::Rgb([80, 120, 200]))
            .save(&path)
            .unwrap();
        let info = inspect(&path, "synthetic.png").unwrap();
        assert_eq!((info.width, info.height), (Some(640), Some(320)));
        let out = dir.path().join("preview.webp");
        preview(&path, &out, None).unwrap();
        assert_eq!(image::image_dimensions(&out).unwrap(), (480, 240));
        preview(&path, &out, Some(vec![0.1, 0.2, 0.3, 0.4])).unwrap();
        assert_eq!(image::image_dimensions(&out).unwrap(), (192, 192));
        assert!(preview(&path, &out, Some(vec![f32::NAN, 0.1, 0.2, 0.3])).is_err());
        let data = std::fs::read(&path).unwrap();
        std::fs::write(&path, &data[..data.len() / 2]).unwrap();
        assert!(inspect(&path, "truncated.png").is_err());
        std::fs::write(&path, b"not an image").unwrap();
        assert!(inspect(&path, "fake.jpg").is_err());
        assert_eq!(
            inspect(&path, "document.txt").unwrap().media_type,
            "text/plain"
        );
    }
    #[test]
    #[ignore = "requires ffmpeg and ffprobe; generated video only"]
    fn generated_video_duration_and_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("synthetic.mp4");
        let result = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=32x24:r=25",
                "-t",
                "0.4",
                "-an",
                "-c:v",
                "mpeg4",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(result.success());
        let inspected = inspect(&path, "synthetic.mp4").unwrap();
        assert_eq!((inspected.width, inspected.height), (Some(32), Some(24)));
        assert_eq!(inspected.media_type, "video/mp4");
        assert_eq!(inspected.duration_ms, Some(400));
    }
    #[test]
    fn huge_dimensions_and_compressed_pixel_bombs_are_rejected() {
        // A small PNG header advertises dimensions before any large decode can
        // allocate. CRC is valid so rejection exercises allocation/dimension limits.
        fn crc(bytes: &[u8]) -> u32 {
            let mut crc = !0u32;
            for &b in bytes {
                crc ^= u32::from(b);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ (0xedb88320u32.wrapping_mul(crc & 1));
                }
            }
            !crc
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("header.png");
        for (w, h) in [(20000u32, 1u32), (16000, 16000), (8000, 8000)] {
            let mut data = b"\x89PNG\r\n\x1a\n".to_vec();
            data.extend(13u32.to_be_bytes());
            let mut ihdr = b"IHDR".to_vec();
            ihdr.extend(w.to_be_bytes());
            ihdr.extend(h.to_be_bytes());
            ihdr.extend([8, 6, 0, 0, 0]);
            data.extend(&ihdr);
            data.extend(crc(&ihdr).to_be_bytes());
            std::fs::write(&path, data).unwrap();
            assert!(inspect(&path, "bomb.png").is_err());
        }
    }
}
