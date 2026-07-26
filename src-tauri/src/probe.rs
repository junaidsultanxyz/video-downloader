use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;
use tauri_plugin_shell::ShellExt;

/// Everything the preview card needs about a single link, distilled from
/// yt-dlp's verbose JSON dump into a shape the frontend can render directly.
#[derive(Serialize)]
pub struct MediaInfo {
    pub title: String,
    pub uploader: String,
    pub thumbnail: String,
    /// Duration in seconds; 0 when the source does not report one (e.g. lives).
    pub duration: f64,
    /// The yt-dlp extractor key, e.g. "youtube" or "tiktok".
    pub extractor: String,
    pub is_live: bool,
    /// True when the media is served as many small segments (a past or current
    /// livestream, or an HLS/DASH-segmented stream) rather than one file. These
    /// download far more slowly — hundreds of separate requests — so the UI
    /// warns about it.
    pub fragmented: bool,
    pub qualities: Vec<QualityOption>,
}

/// One selectable video quality. Ids are our own stable tokens ("vbest",
/// "v1080") rather than raw yt-dlp format ids, which are noisy and vary by site.
#[derive(Serialize)]
pub struct QualityOption {
    pub id: String,
    pub label: String,
    pub height: u32,
    /// A short human note such as "mp4 · ~124 MB". Sizes carry a leading `~`
    /// because DASH video streams exclude audio, so the merged size is larger.
    pub note: String,
}

/// Probe a URL and return its previewable metadata. Runs the bundled yt-dlp
/// with `-J` (dump a single JSON object) and no playlist expansion.
#[tauri::command]
pub async fn probe_url(app: tauri::AppHandle, url: String) -> Result<MediaInfo, String> {
    let output = app
        .shell()
        .sidecar("yt-dlp")
        .map_err(|e| format!("Couldn't start the download engine: {e}"))?
        .args(["-J", "--no-playlist", "--no-warnings", "--no-colors", &url])
        .output()
        .await
        .map_err(|e| format!("The download engine didn't run: {e}"))?;

    if !output.status.success() {
        return Err(friendly_probe_error(&output.stderr));
    }

    let json: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| "Couldn't read that link. Check the post is public, then try again.".to_string())?;

    Ok(parse_media_info(&json))
}

/// yt-dlp's own version string, for the header status line.
#[tauri::command]
pub async fn engine_version(app: tauri::AppHandle) -> Result<String, String> {
    let output = app
        .shell()
        .sidecar("yt-dlp")
        .map_err(|e| e.to_string())?
        .args(["--version"])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Self-update the bundled yt-dlp (`-U`). Extractors break whenever a target
/// site changes its internals, so keeping the engine current is what keeps
/// downloads working over time.
#[tauri::command]
pub async fn update_engine(app: tauri::AppHandle) -> Result<String, String> {
    let output = app
        .shell()
        .sidecar("yt-dlp")
        .map_err(|e| e.to_string())?
        .args(["-U"])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.trim().to_string())
}

/// Turn yt-dlp's JSON info dictionary into a `MediaInfo`.
fn parse_media_info(json: &Value) -> MediaInfo {
    let title = json
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Untitled")
        .to_string();
    let uploader = json
        .get("uploader")
        .or_else(|| json.get("channel"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let thumbnail = json
        .get("thumbnail")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let duration = json.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
    let extractor = json
        .get("extractor_key")
        .or_else(|| json.get("extractor"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let is_live = json.get("is_live").and_then(Value::as_bool).unwrap_or(false);
    let fragmented = is_fragmented(json);

    let qualities = derive_qualities(json);

    MediaInfo {
        title,
        uploader,
        thumbnail,
        duration,
        extractor,
        is_live,
        fragmented,
        qualities,
    }
}

/// Decide whether a source is delivered in segments. A past livestream is the
/// obvious case (`was_live`), but any video format whose transport is DASH
/// segments or HLS is fragmented too — that is what makes a small file crawl.
fn is_fragmented(json: &Value) -> bool {
    if json.get("was_live").and_then(Value::as_bool).unwrap_or(false)
        || json.get("is_live").and_then(Value::as_bool).unwrap_or(false)
    {
        return true;
    }
    json.get("formats")
        .and_then(Value::as_array)
        .is_some_and(|formats| {
            formats.iter().any(|format| {
                format.get("vcodec").and_then(Value::as_str) != Some("none")
                    && format
                        .get("protocol")
                        .and_then(Value::as_str)
                        .is_some_and(|p| p.contains("dash_segments") || p.contains("m3u8"))
            })
        })
}

/// Build the quality menu from the `formats` array — one entry per distinct
/// video height, sharpest first, each keeping the largest known file size at
/// that height. Every option merges to mp4, so notes read as "mp4 · ~size".
/// When a source exposes no per-height formats we fall back to a fixed ladder so
/// single-URL extractors still offer sensible choices.
fn derive_qualities(json: &Value) -> Vec<QualityOption> {
    // height -> largest filesize seen at that height (None when unknown).
    let mut heights: BTreeMap<u32, Option<f64>> = BTreeMap::new();

    if let Some(formats) = json.get("formats").and_then(Value::as_array) {
        for format in formats {
            // Audio-only formats carry no video track; skip them.
            if format.get("vcodec").and_then(Value::as_str) == Some("none") {
                continue;
            }
            let Some(height) = format.get("height").and_then(Value::as_u64) else {
                continue;
            };
            let height = height as u32;

            let filesize = format
                .get("filesize")
                .and_then(Value::as_f64)
                .or_else(|| format.get("filesize_approx").and_then(Value::as_f64));

            heights
                .entry(height)
                .and_modify(|existing| {
                    // Keep the larger file size we've seen at this height.
                    if filesize.unwrap_or(0.0) > existing.unwrap_or(0.0) {
                        *existing = filesize;
                    }
                })
                .or_insert(filesize);
        }
    }

    if heights.is_empty() {
        // No per-height formats to work with — offer a standard ladder.
        return [1080, 720, 480, 360]
            .into_iter()
            .map(|height| QualityOption {
                id: format!("v{height}"),
                label: format!("{height}p"),
                height,
                note: "mp4".into(),
            })
            .collect();
    }

    // Descending height: sharpest option first, lowest last.
    heights
        .into_iter()
        .rev()
        .map(|(height, filesize)| {
            let note = match filesize {
                Some(size) => format!("mp4 · ~{}", human_size(size)),
                None => "mp4".into(),
            };
            QualityOption {
                id: format!("v{height}"),
                label: format!("{height}p"),
                height,
                note,
            }
        })
        .collect()
}

/// Format a byte count as a compact, human-readable size ("124 MB").
fn human_size(bytes: f64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Reduce yt-dlp's stderr to a single readable line, with a nudge toward the
/// "Update engine" button when the failure looks like a stale extractor.
fn friendly_probe_error(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .rev()
        .find(|l| l.contains("ERROR") || l.to_lowercase().contains("error"))
        .unwrap_or_else(|| text.lines().last().unwrap_or(""))
        .trim()
        .trim_start_matches("ERROR:")
        .trim()
        .to_string();

    let lowered = line.to_lowercase();
    if lowered.contains("unsupported url") || lowered.contains("unable to extract") {
        return format!(
            "{line}\n\nThe site may have changed — try \"Update engine\" and probe again."
        );
    }
    if line.is_empty() {
        return "Couldn't read that link. Check the post is public, then try again.".into();
    }
    line
}
