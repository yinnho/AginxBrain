//! Reference media for video requests — Seedance 2.0 Fast omni-reference.
//!
//! A video request arrives as an OpenAI chat body; the media ride in the user
//! message's content array next to the text prompt:
//!
//! ```text
//! {"type":"image_url","image_url":{"url":…},"role":"reference_image"}
//! {"type":"video_url","video_url":{"url":…},"role":"reference_video"}
//! {"type":"audio_url","audio_url":{"url":…},"role":"reference_audio"}
//! ```
//!
//! That block shape IS Volcano Ark's `content[]` wire shape, so "forward
//! as-is" is literal on the Ark route, and the pixmax route maps the same
//! blocks onto canvas asset nodes — one client contract, two backends.
//!
//! This module is the single home of the parsing, the mode rules (首帧 /
//! 首尾帧 / 全模态参考 are mutually exclusive) and the Seedance 2.0 Fast
//! reference-audio limits. Everything here is a pure function — no network —
//! so the contract is unit-tested end to end; proxy.rs owns the I/O.

use serde_json::Value;

// ─── Seedance 2.0 Fast reference-audio limits ──────────────────────────────
//
// Deliberately stricter than Seedance 2.5 (which takes audio alone, more
// clips, longer ones): the pipeline targets 2.0 Fast, and a request that only
// 2.5 would accept must fail here, not at the provider.

/// Reference audio clips per request.
pub const REF_AUDIO_MAX_CLIPS: usize = 3;
/// Duration of one clip, seconds.
pub const REF_AUDIO_MIN_SECS: f64 = 2.0;
pub const REF_AUDIO_MAX_SECS: f64 = 15.0;
/// Sum of all clips, seconds.
pub const REF_AUDIO_TOTAL_SECS: f64 = 15.0;
/// One clip, decoded bytes (Ark and pixmax both state 15 MB = 15728640).
pub const REF_AUDIO_MAX_BYTES: usize = 15 * 1024 * 1024;
/// Whole Ark request body, base64 media included.
pub const SEEDANCE_MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
/// Omni-reference images / videos per request (Ark Seedance 2.0 series).
pub const REF_IMAGE_MAX: usize = 9;
pub const REF_VIDEO_MAX: usize = 3;

/// Why a video request cannot be served, and what the caller's failover loop
/// should do about it.
#[derive(Debug, Clone, PartialEq)]
pub enum MediaError {
    /// The request violates the contract on EVERY route (modes mixed, a clip
    /// over the limit, a malformed block): answer 400 at once.
    Invalid(String),
    /// This model cannot serve an otherwise well-formed request (it renders
    /// no 1080p, …): try the next route; answer 400 when none can.
    Unsupported(String),
}

/// One media block of the client request: its `*_url.url` and its `role`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRef {
    pub url: String,
    pub role: Option<String>,
}

/// Every media block of a video request, in client order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VideoMedia {
    pub images: Vec<MediaRef>,
    pub videos: Vec<MediaRef>,
    pub audios: Vec<MediaRef>,
    /// video_url / audio_url blocks that carry no usable url. Image blocks
    /// without a url stay ignored (legacy behaviour); new block kinds are
    /// reported, because dropping a reference the client meant to send would
    /// submit a different video on their bill.
    pub malformed: Vec<String>,
}

/// Collect the image / video / audio blocks of every message, in order.
/// Same block shapes as `collect_image_urls`: `{"audio_url":"…"}` and
/// `{"audio_url":{"url":"…"}}` both work.
pub fn parse_video_media(body: &Value) -> VideoMedia {
    let mut media = VideoMedia::default();
    let Some(messages) = body.get("messages").and_then(|m| m.as_array()) else {
        return media;
    };
    for msg in messages {
        let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) else {
            continue;
        };
        for (i, block) in blocks.iter().enumerate() {
            let role = block
                .get("role")
                .and_then(|r| r.as_str())
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .map(String::from);
            let targets: [(&str, &mut Vec<MediaRef>); 3] = [
                ("image_url", &mut media.images),
                ("video_url", &mut media.videos),
                ("audio_url", &mut media.audios),
            ];
            for (kind, target) in targets {
                let declared = block.get("type").and_then(|t| t.as_str()) == Some(kind);
                if block.get(kind).is_none() {
                    if declared && kind != "image_url" {
                        media.malformed.push(format!(
                            "{} block #{} in a message content array declares no {}", kind, i + 1, kind
                        ));
                    }
                    continue;
                }
                match block_url(block, kind) {
                    Some(url) => target.push(MediaRef { url, role: role.clone() }),
                    None if kind == "image_url" => {}
                    None => media.malformed.push(format!(
                        "{} block #{} in a message content array has an empty or malformed url",
                        kind, i + 1
                    )),
                }
            }
        }
    }
    media
}

fn block_url(block: &Value, key: &str) -> Option<String> {
    block
        .get(key)
        .and_then(|v| match v {
            Value::String(s) => Some(s.as_str()),
            other => other.get("url").and_then(|u| u.as_str()),
        })
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Which of the Seedance 2.0 modes a request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoRefPlan {
    /// Prompt only.
    Text,
    /// One first-frame image. Role-less images are ALWAYS this — several of
    /// them included (legacy usage: the first drives the video) — so a
    /// role-less image can never be mistaken for a reference image.
    FirstFrame,
    /// `first_frame` + `last_frame`.
    FirstLastFrame,
    /// 全模态参考: `reference_image` ×n, `reference_video` ×n, `reference_audio` ×n.
    Omni,
}

/// Classify a request's media and reject anything the three modes cannot
/// express. 首帧 / 首尾帧 / 全模态参考 are mutually exclusive on Ark, and a
/// mix is always a client slip — e.g. reference audio next to a role-less
/// image — so it is an `Invalid`, never silently reinterpreted.
fn role_of(m: &MediaRef) -> &str {
    m.role.as_deref().unwrap_or("")
}

pub fn plan_video_refs(media: &VideoMedia) -> Result<VideoRefPlan, MediaError> {
    if !media.malformed.is_empty() {
        return Err(MediaError::Invalid(format!(
            "malformed media block: {}",
            media.malformed.join("; ")
        )));
    }
    for img in &media.images {
        if let Some(r) = &img.role {
            if !matches!(r.as_str(), "first_frame" | "last_frame" | "reference_image") {
                return Err(MediaError::Invalid(format!(
                    "unknown image_url role \"{}\" — use \"first_frame\", \"last_frame\", \"reference_image\", \
                     or omit role for a single first frame",
                    r
                )));
            }
        }
    }

    // Any reference-role media puts the whole request in omni-reference mode.
    let omni = media.images.iter().any(|i| role_of(i) == "reference_image")
        || !media.videos.is_empty()
        || !media.audios.is_empty();
    if !omni {
        let describe = || {
            let roles: Vec<String> = media
                .images
                .iter()
                .map(|i| format!("{:?}", i.role.clone().unwrap_or_default()))
                .collect();
            format!("got {} image(s) with roles [{}]", roles.len(), roles.join(", "))
        };
        return match media.images.as_slice() {
            [] => Ok(VideoRefPlan::Text),
            [single] if matches!(role_of(single), "" | "first_frame") => Ok(VideoRefPlan::FirstFrame),
            many if many.iter().all(|i| i.role.is_none()) => Ok(VideoRefPlan::FirstFrame),
            [a, b]
                if matches!(
                    (role_of(a), role_of(b)),
                    ("first_frame", "last_frame") | ("last_frame", "first_frame")
                ) =>
            {
                Ok(VideoRefPlan::FirstLastFrame)
            }
            _ => Err(MediaError::Invalid(format!(
                "first frame and first+last frame are two different usages: pass ONE image (no role, or \
                 role \"first_frame\"), or TWO images with roles \"first_frame\" and \"last_frame\" — {}",
                describe()
            ))),
        };
    }

    if let Some(img) = media
        .images
        .iter()
        .find(|i| matches!(role_of(i), "" | "first_frame" | "last_frame"))
    {
        let why = match &img.role {
            None => "an image_url without role is a first frame, not a reference image".to_string(),
            Some(r) => format!("an image_url with role \"{r}\" belongs to the first-frame / first+last-frame usage"),
        };
        return Err(MediaError::Invalid(format!(
            "{why} — first frame, first+last frame and omni-reference are mutually exclusive; alongside \
             reference audio / video / images every image must carry \"role\": \"reference_image\""
        )));
    }
    for v in &media.videos {
        let got = match &v.role {
            None => "it is required".to_string(),
            Some(r) => format!("got \"{r}\""),
        };
        if role_of(v) != "reference_video" {
            return Err(MediaError::Invalid(format!("video_url role must be \"reference_video\" ({got})")));
        }
    }
    if media.images.len() > REF_IMAGE_MAX {
        return Err(MediaError::Invalid(format!(
            "at most {} reference images per request (got {})",
            REF_IMAGE_MAX,
            media.images.len()
        )));
    }
    if media.videos.len() > REF_VIDEO_MAX {
        return Err(MediaError::Invalid(format!(
            "at most {} reference videos per request (got {})",
            REF_VIDEO_MAX,
            media.videos.len()
        )));
    }
    if media.audios.len() > REF_AUDIO_MAX_CLIPS {
        return Err(MediaError::Invalid(format!(
            "at most {} reference audio clips per request (got {})",
            REF_AUDIO_MAX_CLIPS,
            media.audios.len()
        )));
    }
    if !media.audios.is_empty() && media.images.is_empty() && media.videos.is_empty() {
        return Err(MediaError::Invalid(
            "reference audio cannot be used alone — omni-reference needs at least 1 reference image \
             (\"role\": \"reference_image\") or 1 reference video (\"role\": \"reference_video\") \
             alongside the audio"
                .into(),
        ));
    }
    Ok(VideoRefPlan::Omni)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Wav,
    Mp3,
}

impl AudioFormat {
    pub fn name(&self) -> &'static str {
        match self {
            AudioFormat::Wav => "wav",
            AudioFormat::Mp3 => "mp3",
        }
    }

    /// Browser content type for the pixmax asset upload.
    pub fn mime(&self) -> &'static str {
        match self {
            AudioFormat::Wav => "audio/wav",
            AudioFormat::Mp3 => "audio/mpeg",
        }
    }
}

/// A reference audio clip. All three kinds Ark accepts pass; `url()` is what
/// goes on the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioRef {
    /// Public http(s) URL — forwarded untouched (Ark downloads it itself).
    Url(String),
    /// `data:audio/<wav|mp3>;base64,…`, decoded. `url` is the same data URL
    /// with the format lowercased, which is the spelling Ark requires.
    Data { url: String, fmt: AudioFormat, bytes: Vec<u8> },
    /// `asset://<ASSET_ID>` — a Volcano digital-human audio asset.
    Asset(String),
}

impl AudioRef {
    pub fn url(&self) -> &str {
        match self {
            AudioRef::Url(u) => u,
            AudioRef::Data { url, .. } => url,
            AudioRef::Asset(id) => id,
        }
    }
}

/// Decode one base64 payload, tolerating line wraps and a missing padding.
fn decode_base64(payload: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(payload).or_else(|_| {
        let padded = match payload.len() % 4 {
            0 => payload.to_string(),
            n => format!("{}{}", payload, "=".repeat(4 - n)),
        };
        base64::engine::general_purpose::STANDARD.decode(padded)
    })
}

/// Parse an `audio_url.url` of any of the three accepted kinds.
pub fn parse_audio_ref(url: &str) -> Result<AudioRef, String> {
    let url = url.trim();
    if let Some(id) = url.strip_prefix("asset://") {
        if id.is_empty() {
            return Err("asset:// reference carries no asset id".into());
        }
        return Ok(AudioRef::Asset(url.to_string()));
    }
    if let Some(rest) = url.strip_prefix("data:") {
        let Some((meta, payload)) = rest.split_once(',') else {
            return Err("data URL has no base64 payload after a comma".into());
        };
        let mut params = meta.split(';');
        let mime = params.next().unwrap_or("").trim().to_ascii_lowercase();
        if !params.any(|p| p.trim().eq_ignore_ascii_case("base64")) {
            return Err(format!(
                "data URL must be base64 (\"data:audio/wav;base64,…\"), got \"{}\"",
                truncate(meta, 40)
            ));
        }
        let fmt = match mime.as_str() {
            "audio/wav" | "audio/x-wav" | "audio/wave" => AudioFormat::Wav,
            "audio/mp3" | "audio/mpeg" => AudioFormat::Mp3,
            "" => return Err("data URL has no audio format (use data:audio/wav or data:audio/mp3)".into()),
            other => {
                return Err(format!(
                    "unsupported audio format \"{}\" — reference audio accepts wav and mp3 only",
                    other
                ))
            }
        };
        let payload: String = payload.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        if payload.is_empty() {
            return Err("data URL carries no audio".into());
        }
        // 4 base64 chars per 3 bytes: refuse an oversized clip before
        // allocating its decoded form.
        if payload.len() > (REF_AUDIO_MAX_BYTES + 2).saturating_mul(4) / 3 {
            return Err(format!(
                "the clip is over the {} MB per-clip limit — send it as a public URL instead of base64",
                REF_AUDIO_MAX_BYTES / 1024 / 1024
            ));
        }
        let bytes = decode_base64(&payload).map_err(|e| format!("invalid base64 audio: {e}"))?;
        let url = format!("data:audio/{};base64,{}", fmt.name(), payload);
        return Ok(AudioRef::Data { url, fmt, bytes });
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return Ok(AudioRef::Url(url.to_string()));
    }
    Err(format!(
        "unsupported audio url \"{}\" — use a public http(s) URL, data:audio/<wav|mp3>;base64,… \
         or asset://<ASSET_ID>",
        truncate(url, 40)
    ))
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Identify an audio file by its magic bytes.
pub fn sniff_audio_format(bytes: &[u8]) -> Option<AudioFormat> {
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        return Some(AudioFormat::Wav);
    }
    let mp3 = bytes.len() >= 3
        && (bytes.starts_with(b"ID3") || (bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0));
    mp3.then_some(AudioFormat::Mp3)
}

/// The per-clip size error. `bytes` is the clip's size when it is known (a
/// fetch that hit the cap only knows "more than the limit"). `inline` = the
/// clip arrived as base64, in which case a URL is the way out; a clip that
/// already came by URL has to shrink.
pub fn audio_too_large_error(index: usize, bytes: Option<usize>, inline: bool) -> MediaError {
    let way_out = if inline {
        " — send it as a public URL instead of base64"
    } else {
        " — use a shorter or better compressed clip"
    };
    let limit = REF_AUDIO_MAX_BYTES / 1024 / 1024;
    let size = match bytes {
        Some(n) => format!("{:.1} MB is over the", n as f64 / 1024.0 / 1024.0),
        None => "over the".to_string(),
    };
    MediaError::Invalid(format!(
        "reference audio #{index}: {size} {limit} MB per-clip limit{way_out}"
    ))
}

/// Check one clip's bytes against the Seedance 2.0 Fast limits and return its
/// duration. `inline` = the clip arrived as base64, i.e. a URL is the way out.
pub fn check_audio_clip(
    index: usize,
    declared: AudioFormat,
    bytes: &[u8],
    inline: bool,
) -> Result<ClipDuration, MediaError> {
    let invalid = |msg: String| MediaError::Invalid(format!("reference audio #{index}: {msg}"));
    if bytes.len() > REF_AUDIO_MAX_BYTES {
        return Err(audio_too_large_error(index, Some(bytes.len()), inline));
    }
    let Some(actual) = sniff_audio_format(bytes) else {
        return Err(invalid(
            "unrecognized audio data — reference audio accepts wav and mp3 only".into(),
        ));
    };
    if actual != declared {
        return Err(invalid(format!(
            "declared as {} but the audio data is {}",
            declared.name(),
            actual.name()
        )));
    }
    let Some(clip) = audio_duration(bytes) else {
        return Err(invalid(
            "could not read the clip's duration — reference audio accepts wav and mp3 only".into(),
        ));
    };
    // The container may overcount by `overhead`; beyond that the clip is
    // really over the limit. Under the limit the container never undercounts.
    if clip.secs < REF_AUDIO_MIN_SECS {
        return Err(invalid(format!("{:.2}s is shorter than the {}s minimum", clip.secs, REF_AUDIO_MIN_SECS as u32)));
    }
    if clip.secs > REF_AUDIO_MAX_SECS + clip.overhead {
        return Err(invalid(format!("{:.2}s is longer than the {}s maximum", clip.secs, REF_AUDIO_MAX_SECS as u32)));
    }
    Ok(clip)
}

/// Check the total of clips whose duration is known (everything the gateway
/// holds bytes for); clips only referenced by URL are left to the provider.
pub fn check_audio_total(secs: f64, overhead: f64) -> Result<(), MediaError> {
    if secs > REF_AUDIO_TOTAL_SECS + overhead {
        return Err(MediaError::Invalid(format!(
            "the reference audio adds up to {secs:.2}s — all clips together must stay within {}s \
             (up to {} clips)",
            REF_AUDIO_TOTAL_SECS as u32, REF_AUDIO_MAX_CLIPS
        )));
    }
    Ok(())
}

/// Validate a request that carries reference audio against the Seedance 2.0
/// Fast contract: mode rules (audio never alone, never next to first frames,
/// images must be `reference_image`), clip count, format, size and duration.
/// Runs before any route work — these fail on every route, so there is
/// nothing to fail over to.
pub fn validate_reference_audio(media: &VideoMedia) -> Result<(), MediaError> {
    plan_video_refs(media)?;
    let mut total = 0.0;
    let mut overhead = 0.0;
    for (i, audio) in media.audios.iter().enumerate() {
        if let Some(r) = &audio.role {
            if r != "reference_audio" {
                return Err(MediaError::Invalid(format!(
                    "audio_url role is fixed to \"reference_audio\" (clip #{} says \"{}\")",
                    i + 1,
                    r
                )));
            }
        }
        match parse_audio_ref(&audio.url) {
            Ok(AudioRef::Data { fmt, bytes, .. }) => {
                let clip = check_audio_clip(i + 1, fmt, &bytes, true)?;
                total += clip.secs;
                overhead += clip.overhead;
            }
            Ok(_) => {}
            Err(e) => return Err(MediaError::Invalid(format!("reference audio #{}: {e}", i + 1))),
        }
    }
    check_audio_total(total, overhead)
}

// ─── Audio duration from the container ─────────────────────────────────────
//
// A gateway has no decoder, but a clip's duration is in its header: WAV
// states the byte rate, MP3 stacks fixed-size frames. ~100 lines beat an
// ffprobe dependency on the server (and pixmax's own probe runs only after
// the upload, i.e. after the client has waited).

/// A clip's length as the gateway can see it from the container.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipDuration {
    /// Seconds of audio the container holds.
    pub secs: f64,
    /// How far `secs` may exceed what a decoder — and so the provider —
    /// reports. WAV is exact (0). MP3 encoders prepend ~1105 samples of
    /// delay and pad the tail with up to a couple of frames, which a decoder
    /// trims: a LAME-encoded 15.000 s clip counts as 15.05 s of frames at
    /// 44.1 kHz and 15.18 s at 8 kHz. Measured against ffprobe over CBR/VBR,
    /// mono/stereo, 8–48 kHz clips.
    pub overhead: f64,
}

/// Duration of a wav / mp3 clip, from the container alone.
pub fn audio_duration(bytes: &[u8]) -> Option<ClipDuration> {
    match sniff_audio_format(bytes)? {
        AudioFormat::Wav => wav_duration_secs(bytes).map(|secs| ClipDuration { secs, overhead: 0.0 }),
        AudioFormat::Mp3 => mp3_duration(bytes),
    }
}

fn wav_duration_secs(b: &[u8]) -> Option<f64> {
    let u16_at = |p: usize| b.get(p..p + 2).map(|x| u16::from_le_bytes([x[0], x[1]]));
    let u32_at = |p: usize| b.get(p..p + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]));
    let mut pos = 12usize;
    let mut fmt_chunk: Option<(u32, u16, u16, u32)> = None; // sample_rate, channels, bits, byte_rate
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let size = u32_at(pos + 4)? as usize;
        let body = pos + 8;
        if id == b"fmt " && body + 16 <= b.len() {
            fmt_chunk = Some((u32_at(body + 4)?, u16_at(body + 2)?, u16_at(body + 14)?, u32_at(body + 8)?));
        } else if id == b"data" {
            let available = b.len().saturating_sub(body);
            // Streaming writers leave 0 / 0xFFFFFFFF here: trust the real bytes.
            let data_len = if size == 0 || size > available { available } else { size };
            let (rate, channels, bits, byte_rate) = fmt_chunk?;
            let bytes_per_sec = match byte_rate {
                0 if rate > 0 && channels > 0 && bits > 0 => {
                    (u32::from(rate) * u32::from(channels) * u32::from(bits) / 8) as usize
                }
                0 => return None,
                v => v as usize,
            };
            return Some(data_len as f64 / bytes_per_sec as f64);
        }
        pos = body.saturating_add(size).saturating_add(size & 1);
    }
    None
}

struct Mp3Frame {
    len: usize,
    samples: u32,
    sample_rate: u32,
}

const MP3_BITRATES_V1: [u32; 16] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0];
const MP3_BITRATES_V2: [u32; 16] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0];

/// Decode a MPEG audio Layer III frame header (4 bytes).
fn parse_mp3_frame(b: &[u8]) -> Option<Mp3Frame> {
    if b.len() < 4 || b[0] != 0xFF || b[1] & 0xE0 != 0xE0 {
        return None;
    }
    // 2 bits each: version (3 = MPEG1, 2 = MPEG2, 0 = MPEG2.5), layer (1 = III).
    let version = match (b[1] >> 3) & 0x03 {
        3 => 1,
        2 => 2,
        0 => 25,
        _ => return None,
    };
    if (b[1] >> 1) & 0x03 != 1 {
        return None;
    }
    let bitrate_idx = ((b[2] >> 4) & 0x0F) as usize;
    let rate_idx = ((b[2] >> 2) & 0x03) as usize;
    if bitrate_idx == 0 || bitrate_idx == 15 || rate_idx == 3 {
        return None;
    }
    // Bitrate table, sample rates and samples per frame all follow the version:
    // MPEG1 has 1152 samples per frame, MPEG2/2.5 have 576 (→ 144 / 72 bytes of
    // bitrate-scaled frame length).
    let (bitrate, sample_rate, samples) = match version {
        1 => (MP3_BITRATES_V1[bitrate_idx], [44100, 48000, 32000][rate_idx], 1152),
        2 => (MP3_BITRATES_V2[bitrate_idx], [22050, 24000, 16000][rate_idx], 576),
        _ => (MP3_BITRATES_V2[bitrate_idx], [11025, 12000, 8000][rate_idx], 576),
    };
    let padding = usize::from((b[2] >> 1) & 0x01);
    let len = ((samples / 8 * bitrate * 1000) / sample_rate) as usize + padding;
    Some(Mp3Frame { len, samples, sample_rate })
}

/// Encoder delay every LAME-derived encoder adds (576 encoder + 529 decoder).
const MP3_ENCODER_DELAY_SAMPLES: u32 = 1105;

fn mp3_duration(b: &[u8]) -> Option<ClipDuration> {
    let mut pos = 0usize;
    if b.starts_with(b"ID3") && b.len() >= 10 {
        // ID3v2 size: 4 × 7-bit syncsafe bytes; a footer doubles the tag frame.
        let size = ((b[6] as usize & 0x7F) << 21)
            | ((b[7] as usize & 0x7F) << 14)
            | ((b[8] as usize & 0x7F) << 7)
            | (b[9] as usize & 0x7F);
        pos = 10 + size + if b[5] & 0x10 != 0 { 10 } else { 0 };
    }
    let mut secs = 0f64;
    let mut frames = 0usize;
    let mut frame_shape: Option<(u32, u32)> = None; // samples, sample_rate
    // Before the first frame, byte-synchronise over junk (a tag whose size we
    // misread) — but only within this window, so a non-MP3 is not scanned whole.
    const SYNC_WINDOW: usize = 64 * 1024;
    let audio_start = pos;
    while pos + 4 <= b.len() {
        let Some(frame) = parse_mp3_frame(&b[pos..]) else {
            if frames == 0 && pos - audio_start < SYNC_WINDOW {
                pos += 1;
                continue;
            }
            // After real audio, a bad header ends the stream (trailing ID3v1 tag).
            break;
        };
        if frame.len == 0 || pos + frame.len > b.len() {
            break;
        }
        // A Xing/Info header is a valid frame that carries no audio.
        let stereo = (b[pos + 3] >> 6) & 0x03 != 3;
        let mpeg1 = (b[pos + 1] >> 3) & 0x03 == 3;
        let tag_at = pos + 4 + match (mpeg1, stereo) {
            (true, true) => 32,
            (true, false) => 17,
            (false, true) => 17,
            (false, false) => 9,
        };
        let is_info_frame = b.get(tag_at..tag_at + 4).is_some_and(|t| t == b"Xing" || t == b"Info");
        if !is_info_frame {
            secs += frame.samples as f64 / frame.sample_rate as f64;
            frame_shape.get_or_insert((frame.samples, frame.sample_rate));
        }
        frames += 1;
        pos += frame.len;
    }
    // One frame is 26 ms of audio — never a valid clip, so a lone frame is a false sync.
    let (samples, sample_rate) = frame_shape.filter(|_| frames >= 2)?;
    Some(ClipDuration {
        secs,
        overhead: (MP3_ENCODER_DELAY_SAMPLES + 2 * samples) as f64 / sample_rate as f64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use serde_json::json;

    fn user(blocks: Value) -> Value {
        json!({ "messages": [{ "role": "user", "content": blocks }] })
    }

    fn img(url: &str, role: Option<&str>) -> Value {
        let mut v = json!({"type": "image_url", "image_url": {"url": url}});
        if let Some(r) = role {
            v["role"] = json!(r);
        }
        v
    }

    fn audio(url: &str) -> Value {
        json!({"type": "audio_url", "audio_url": {"url": url}, "role": "reference_audio"})
    }

    /// PCM16 WAV: `secs` of stereo silence at 44.1 kHz.
    fn wav(secs: f64) -> Vec<u8> {
        let data_len = (secs * 44100.0 * 4.0) as usize;
        let mut b = Vec::with_capacity(44 + data_len);
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        b.extend_from_slice(b"WAVE");
        b.extend_from_slice(b"fmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // PCM
        b.extend_from_slice(&2u16.to_le_bytes()); // channels
        b.extend_from_slice(&44100u32.to_le_bytes()); // sample rate
        b.extend_from_slice(&176400u32.to_le_bytes()); // byte rate
        b.extend_from_slice(&4u16.to_le_bytes()); // block align
        b.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data_len as u32).to_le_bytes());
        b.resize(44 + data_len, 0);
        b
    }

    /// MPEG1 Layer III: 128 kbps @ 44.1 kHz stereo, 1152 samples per 417-byte
    /// frame, optionally behind an ID3v2 tag and/or behind a Xing info frame.
    fn mp3(frames: usize, id3: bool, xing: bool) -> Vec<u8> {
        let header = [0xFFu8, 0xFB, 0x90, 0x00];
        let frame_len = 144 * 128_000 / 44100; // 417
        let mut b = Vec::new();
        if id3 {
            b.extend_from_slice(b"ID3\x04\x00\x00\x00\x00\x00\x0A"); // syncsafe size = 10
            b.resize(20, 0);
        }
        if xing {
            b.extend_from_slice(&header);
            b.resize(b.len() + 32, 0); // side info
            b.extend_from_slice(b"Xing");
            b.resize(b.len() + frame_len - 40, 0);
        }
        for _ in 0..frames {
            b.extend_from_slice(&header);
            b.resize(b.len() + frame_len - 4, 0);
        }
        b
    }

    fn data_url(fmt: &str, bytes: &[u8]) -> String {
        format!(
            "data:audio/{};base64,{}",
            fmt,
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    fn plan(blocks: Value) -> Result<VideoRefPlan, MediaError> {
        plan_video_refs(&parse_video_media(&user(blocks)))
    }

    fn validate(blocks: Value) -> Result<(), MediaError> {
        validate_reference_audio(&parse_video_media(&user(blocks)))
    }

    #[test]
    fn parse_collects_every_media_kind_in_order() {
        let body = json!({"messages": [
            {"role": "user", "content": [
                {"type": "text", "text": "go"},
                img("https://x/a.png", Some("reference_image")),
                {"type": "audio_url", "audio_url": "https://x/v.mp3"},
                {"type": "video_url", "video_url": {"url": "https://x/v.mp4"}, "role": "reference_video"}
            ]},
            {"role": "assistant", "content": "ok"},
            {"role": "user", "content": [
                img("https://x/b.png", Some("reference_image")),
                {"type": "image_url", "image_url": {"url": "   "}}
            ]}
        ]});
        let m = parse_video_media(&body);
        assert_eq!(
            m.images,
            vec![
                MediaRef { url: "https://x/a.png".into(), role: Some("reference_image".into()) },
                MediaRef { url: "https://x/b.png".into(), role: Some("reference_image".into()) },
            ]
        );
        assert_eq!(m.videos.len(), 1);
        assert_eq!(m.videos[0].role.as_deref(), Some("reference_video"));
        assert_eq!(m.audios, vec![MediaRef { url: "https://x/v.mp3".into(), role: None }]);
        // a blank image url stays ignored (legacy); malformed new kinds are reported
        assert!(m.malformed.is_empty());

        let bad = parse_video_media(&user(json!([{"type": "audio_url", "role": "reference_audio"}])));
        assert!(bad.malformed[0].contains("audio_url"), "{}", bad.malformed[0]);
        let no_url = parse_video_media(&user(json!([{"type": "video_url", "video_url": {"url": ""}}])));
        assert!(no_url.malformed[0].contains("video_url"));
        assert!(parse_video_media(&json!({"messages": [{"role": "user", "content": "plain"}]})).images.is_empty());
        assert!(parse_video_media(&json!({})).audios.is_empty());
    }

    #[test]
    fn plan_first_frame_usage() {
        assert_eq!(plan(json!([])).unwrap(), VideoRefPlan::Text);
        assert_eq!(plan(json!([img("https://x/a.png", None)])).unwrap(), VideoRefPlan::FirstFrame);
        assert_eq!(plan(json!([img("https://x/a.png", Some("first_frame"))])).unwrap(), VideoRefPlan::FirstFrame);
        // legacy multi-image: role-less images are first-frame usage, never references
        assert_eq!(
            plan(json!([img("https://x/a.png", None), img("https://x/b.png", None)])).unwrap(),
            VideoRefPlan::FirstFrame
        );
        assert_eq!(
            plan(json!([img("https://x/first.png", Some("first_frame")), img("https://x/last.png", Some("last_frame"))]))
                .unwrap(),
            VideoRefPlan::FirstLastFrame
        );

        // two first frames / a lone last frame / one framed + one role-less are client slips
        for blocks in [
            json!([img("https://x/a.png", Some("first_frame")), img("https://x/b.png", Some("first_frame"))]),
            json!([img("https://x/a.png", Some("last_frame"))]),
            json!([img("https://x/a.png", None), img("https://x/b.png", Some("last_frame"))]),
        ] {
            let e = plan(blocks).unwrap_err();
            assert!(matches!(&e, MediaError::Invalid(m) if m.contains("TWO images")), "{e:?}");
        }
        let typo = plan(json!([img("https://x/a.png", Some("reference-image"))])).unwrap_err();
        assert!(matches!(&typo, MediaError::Invalid(m) if m.contains("unknown image_url role")), "{typo:?}");
    }

    #[test]
    fn plan_omni_reference_rules() {
        // images / videos alone are omni-reference too (audio is optional)
        assert_eq!(plan(json!([img("https://x/a.png", Some("reference_image"))])).unwrap(), VideoRefPlan::Omni);
        assert_eq!(
            plan(json!([{"type": "video_url", "video_url": {"url": "https://x/v.mp4"}, "role": "reference_video"}]))
                .unwrap(),
            VideoRefPlan::Omni
        );
        assert_eq!(
            plan(json!([audio("https://x/v.mp3"), img("https://x/a.png", Some("reference_image"))])).unwrap(),
            VideoRefPlan::Omni
        );

        // audio alone is a 4xx (Seedance 2.5 allows it — 2.0 Fast does not)
        let alone = plan(json!([audio("https://x/v.mp3")])).unwrap_err();
        assert!(matches!(&alone, MediaError::Invalid(m) if m.contains("cannot be used alone")), "{alone:?}");

        // a role-less image is a first frame: never promoted to a reference
        let roleless = plan(json!([audio("https://x/v.mp3"), img("https://x/a.png", None)])).unwrap_err();
        assert!(matches!(&roleless, MediaError::Invalid(m) if m.contains("mutually exclusive")), "{roleless:?}");
        for frame_role in ["first_frame", "last_frame"] {
            let mixed = plan(json!([audio("https://x/v.mp3"), img("https://x/a.png", Some(frame_role))])).unwrap_err();
            assert!(matches!(&mixed, MediaError::Invalid(m) if m.contains("mutually exclusive")), "{mixed:?}");
        }

        // videos must say reference_video
        assert!(plan(json!([{"type": "video_url", "video_url": {"url": "https://x/v.mp4"}}])).is_err());
        assert!(plan(json!([{"type": "video_url", "video_url": {"url": "https://x/v.mp4"}, "role": "first_frame"}]))
            .is_err());

        // caps: 9 images, 3 videos, 3 audio clips
        let images = |n: usize| -> Vec<Value> {
            (0..n).map(|i| img(&format!("https://x/{i}.png"), Some("reference_image"))).collect()
        };
        assert_eq!(plan(json!(images(9))).unwrap(), VideoRefPlan::Omni);
        let ten = plan(json!(images(10))).unwrap_err();
        assert!(matches!(&ten, MediaError::Invalid(m) if m.contains("at most 9 reference images")), "{ten:?}");

        let video = || json!({"type": "video_url", "video_url": {"url": "https://x/v.mp4"}, "role": "reference_video"});
        let mut three_v: Vec<Value> = images(1);
        three_v.extend((0..3).map(|_| video()));
        assert_eq!(plan(json!(three_v)).unwrap(), VideoRefPlan::Omni);
        let mut four_v: Vec<Value> = images(1);
        four_v.extend((0..4).map(|_| video()));
        assert!(matches!(plan(json!(four_v)), Err(MediaError::Invalid(m)) if m.contains("3 reference videos")));

        let mut three_a: Vec<Value> = images(1);
        three_a.extend((0..3).map(|_| audio("https://x/v.mp3")));
        assert!(validate(json!(three_a)).is_ok());
        let mut four_a: Vec<Value> = images(1);
        four_a.extend((0..4).map(|_| audio("https://x/v.mp3")));
        let four = validate(json!(four_a)).unwrap_err();
        assert!(matches!(&four, MediaError::Invalid(m) if m.contains("3 reference audio")), "{four:?}");
    }

    #[test]
    fn audio_ref_accepts_url_data_and_asset() {
        assert_eq!(parse_audio_ref(" https://x/voice.wav ").unwrap(), AudioRef::Url("https://x/voice.wav".into()));
        // the scheme stays on the wire value
        assert_eq!(parse_audio_ref("asset://a-123").unwrap(), AudioRef::Asset("asset://a-123".into()));
        assert_eq!(parse_audio_ref("asset://a-123").unwrap().url(), "asset://a-123");
        assert_eq!(parse_audio_ref("asset://").unwrap_err(), "asset:// reference carries no asset id");

        let bytes = wav(3.0);
        let d = parse_audio_ref(&data_url("wav", &bytes)).unwrap();
        assert_eq!(
            d,
            AudioRef::Data { url: data_url("wav", &bytes), fmt: AudioFormat::Wav, bytes: bytes.clone() }
        );
        // uppercase format, the audio/mpeg alias and line-wrapped base64 are
        // accepted and normalized to the spelling Ark requires
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let wrapped = format!("data:audio/WAV;base64,{}\n{}", &b64[..40], &b64[40..]);
        let d = parse_audio_ref(&wrapped).unwrap();
        assert_eq!(d, AudioRef::Data { url: data_url("wav", &bytes), fmt: AudioFormat::Wav, bytes });
        let mpeg = parse_audio_ref(&data_url("mpeg", &mp3(100, false, false))).unwrap();
        assert_eq!(mpeg.url(), &data_url("mp3", &mp3(100, false, false)));

        for (url, needle) in [
            ("data:audio/ogg;base64,AAAA", "wav and mp3 only"),
            ("data:audio/wav,abc", "must be base64"),
            ("data:;base64,AAAA", "no audio format"),
            ("ftp://x/voice.wav", "unsupported audio url"),
            ("data:audio/wav;base64,", "carries no audio"),
            ("data:audio/wav;base64,@@@", "invalid base64"),
        ] {
            let e = parse_audio_ref(url).unwrap_err();
            assert!(e.contains(needle), "{url} -> {e}");
        }
        // an oversized base64 clip is refused before it is decoded
        let oversized = "A".repeat((REF_AUDIO_MAX_BYTES + 2) * 4 / 3 + 8);
        let e = parse_audio_ref(&format!("data:audio/mp3;base64,{oversized}")).unwrap_err();
        assert!(e.contains("public URL"), "{e}");
    }

    #[test]
    fn clip_duration_size_and_format_checks() {
        let clip = check_audio_clip(1, AudioFormat::Wav, &wav(2.0), true).unwrap();
        assert_eq!(clip, ClipDuration { secs: 2.0, overhead: 0.0 });
        assert_eq!(check_audio_clip(1, AudioFormat::Wav, &wav(15.0), true).unwrap().secs, 15.0);

        let short = check_audio_clip(2, AudioFormat::Wav, &wav(1.9), true).unwrap_err();
        assert!(matches!(&short, MediaError::Invalid(m) if m.contains("#2") && m.contains("shorter")), "{short:?}");
        let long = check_audio_clip(1, AudioFormat::Wav, &wav(15.5), true).unwrap_err();
        assert!(matches!(&long, MediaError::Invalid(m) if m.contains("longer")), "{long:?}");

        // declared wav, really mp3
        let mismatch = check_audio_clip(1, AudioFormat::Wav, &mp3(100, false, false), true).unwrap_err();
        assert!(matches!(&mismatch, MediaError::Invalid(m) if m.contains("declared as wav")), "{mismatch:?}");
        let junk = check_audio_clip(1, AudioFormat::Mp3, b"not audio at all", true).unwrap_err();
        assert!(matches!(&junk, MediaError::Invalid(m) if m.contains("unrecognized")), "{junk:?}");
        let empty = check_audio_clip(1, AudioFormat::Wav, &wav(0.0), true).unwrap_err();
        assert!(matches!(&empty, MediaError::Invalid(m) if m.contains("shorter")), "{empty:?}");

        // per-clip size: base64 is refused with "use a URL", a fetched URL with "shorter clip"
        let big = vec![0u8; REF_AUDIO_MAX_BYTES + 1];
        let inline = check_audio_clip(1, AudioFormat::Mp3, &big, true).unwrap_err();
        assert!(matches!(&inline, MediaError::Invalid(m) if m.contains("public URL")), "{inline:?}");
        let fetched = check_audio_clip(1, AudioFormat::Mp3, &big, false).unwrap_err();
        assert!(matches!(&fetched, MediaError::Invalid(m) if m.contains("shorter")), "{fetched:?}");

        assert!(check_audio_total(15.0, 0.0).is_ok());
        assert!(check_audio_total(15.05, 0.0).is_err());
        assert!(check_audio_total(15.2, 0.3).is_ok());
    }

    #[test]
    fn validate_reference_audio_end_to_end() {
        let clip = |url: String| json!({"type": "audio_url", "audio_url": {"url": url}, "role": "reference_audio"});
        let reference = img("https://x/a.png", Some("reference_image"));

        assert!(validate(json!([reference, clip(data_url("wav", &wav(5.0)))])).is_ok());
        // the audio block's role may be omitted — it is fixed on the wire anyway
        assert!(validate(json!([reference, {"type": "audio_url", "audio_url": {"url": "https://x/v.mp3"}}])).is_ok());
        // asset:// and public URLs pass through; their duration is the provider's to check
        assert!(validate(json!([reference, audio("asset://voice-1"), audio("https://x/a.mp3"), audio("https://x/b.mp3")]))
            .is_ok());

        let wrong_role = validate(json!([
            reference,
            {"type": "audio_url", "audio_url": {"url": "https://x/v.mp3"}, "role": "reference_voice"}
        ]))
        .unwrap_err();
        assert!(matches!(&wrong_role, MediaError::Invalid(m) if m.contains("fixed to")), "{wrong_role:?}");

        // 2 clips of 8s = 16s > 15s in total
        let over = validate(json!([reference, clip(data_url("wav", &wav(8.0))), clip(data_url("wav", &wav(8.0)))]))
            .unwrap_err();
        assert!(matches!(&over, MediaError::Invalid(m) if m.contains("adds up to 16.00s")), "{over:?}");
        assert!(validate(json!([reference, clip(data_url("wav", &wav(7.5))), clip(data_url("wav", &wav(7.5)))])).is_ok());
        // one clip over the per-clip maximum
        let one_long = validate(json!([reference, clip(data_url("wav", &wav(20.0)))])).unwrap_err();
        assert!(matches!(&one_long, MediaError::Invalid(m) if m.contains("longer")), "{one_long:?}");
    }

    #[test]
    fn wav_duration() {
        assert_eq!(audio_duration(&wav(2.5)).unwrap(), ClipDuration { secs: 2.5, overhead: 0.0 });
        // extra chunk before fmt, and a data size of 0 (streaming writer)
        let src = wav(3.0);
        let mut b = b"RIFF\0\0\0\0WAVELIST\x04\0\0\0INFO".to_vec();
        b.extend_from_slice(&src[12..]);
        let data_len = (3.0 * 44100.0 * 4.0) as usize;
        let size_at = b.len() - data_len - 4;
        b[size_at..size_at + 4].fill(0);
        assert!((audio_duration(&b).unwrap().secs - 3.0).abs() < 1e-9);
        // header only → no audio
        assert_eq!(audio_duration(&wav(0.0)).unwrap().secs, 0.0);
        assert!(audio_duration(b"RIFF\0\0\0\0WAVEfmt ").is_none());
    }

    #[test]
    fn mp3_duration() {
        let frame = 1152.0 / 44100.0;
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(near(audio_duration(&mp3(100, false, false)).unwrap().secs, 100.0 * frame));
        assert!(near(audio_duration(&mp3(100, true, false)).unwrap().secs, 100.0 * frame));
        // the Xing info frame carries no audio
        assert!(near(audio_duration(&mp3(100, false, true)).unwrap().secs, 100.0 * frame));
        assert!(near(audio_duration(&mp3(100, true, true)).unwrap().secs, 100.0 * frame));
        // junk ahead of the audio is not an mp3 to us: the sniff is strict
        let mut lead = vec![0u8; 37];
        lead.extend(mp3(50, false, false));
        assert!(audio_duration(&lead).is_none());
        // a trailing ID3v1 tag is not audio
        let mut tag = mp3(100, false, false);
        tag.extend_from_slice(b"TAG");
        tag.resize(tag.len() + 125, 0);
        assert!(near(audio_duration(&tag).unwrap().secs, 100.0 * frame));
        // a truncated last frame is ignored
        let mut cut = mp3(100, false, false);
        cut.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00, 0x00]);
        assert!(near(audio_duration(&cut).unwrap().secs, 100.0 * frame));
        // MPEG2.5, 8 kbps @ 11.025 kHz: 576 samples per 52-byte frame
        let mut low = Vec::new();
        for _ in 0..400 {
            low.extend_from_slice(&[0xFF, 0xE3, 0x10, 0x00]);
            low.resize(low.len() + 48, 0);
        }
        assert!((audio_duration(&low).unwrap().secs - 400.0 * 576.0 / 11025.0).abs() < 1e-6);
        // a lone frame is a false sync, never a clip
        assert!(audio_duration(&mp3(1, false, false)).is_none());
        assert!(audio_duration(&[0xFF, 0xFB, 0x90, 0x00]).is_none());
    }

    #[test]
    fn clip_limits_on_encoded_streams() {
        // 2.0 s exactly passes; 1.99 s does not.
        assert!(check_audio_clip(1, AudioFormat::Wav, &wav(2.0), true).is_ok());
        assert!(check_audio_clip(1, AudioFormat::Wav, &wav(1.99), true).is_err());
        // 576 frames = what LAME writes for a 15.000 s source (encoder-delay
        // padding included): 15.045 s must still pass; 590 frames (15.27 s) may not.
        let clip = check_audio_clip(1, AudioFormat::Mp3, &mp3(576, true, true), true).unwrap();
        assert!(clip.secs > 15.0 && clip.secs < 15.1, "{clip:?}");
        assert!((clip.overhead - (1105 + 2 * 1152) as f64 / 44100.0).abs() < 1e-9);
        assert!(check_audio_clip(1, AudioFormat::Mp3, &mp3(590, true, true), true).is_err());
        // the same on MPEG2.5 (11.025 kHz, 52-byte frames): 0.15 s of
        // overshoot is still a 15 s clip there.
        let low_bitrate = |frames: usize| {
            let mut low = Vec::new();
            for _ in 0..frames {
                low.extend_from_slice(&[0xFF, 0xE3, 0x10, 0x00]);
                low.resize(low.len() + 48, 0);
            }
            low
        };
        let clip = check_audio_clip(1, AudioFormat::Mp3, &low_bitrate(290), true).unwrap();
        assert!((clip.secs - 15.151).abs() < 0.01 && clip.overhead > 0.2, "{clip:?}");
        assert!(check_audio_clip(1, AudioFormat::Mp3, &low_bitrate(300), true).is_err());
        assert!(check_audio_clip(1, AudioFormat::Mp3, &mp3(600, true, true), true).is_err());
    }
}
