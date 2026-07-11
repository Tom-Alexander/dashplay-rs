//! Write a DASH stream to disk as a progressive MP4 file.
//!
//! Usage:
//! ```text
//! cargo run --example write_stream -- https://dash.akamaized.net/akamai/bbb_30fps/bbb_30fps.mpd bbb_30fps.mp4
//! ```
//!
//! For DRM-protected streams whose MPD does not include a license URL:
//! ```text
//! cargo run --example write_stream -- https://example.com/manifest.mpd output.mp4 \
//!   --license-url https://license.example/wv
//! ```
//!
//! Requires `ffmpeg` on `PATH`. Each adaptation set is collected as a fragmented MP4 byte
//! stream and remuxed into a single progressive MP4 container without re-encoding.

use std::path::{Path, PathBuf};
use std::process::Command;

use dashplayrs::{PlayerEvent, PlayerTrackOutput};
use tokio::sync::broadcast;

#[derive(Debug)]
struct Args {
    manifest_url: String,
    output: PathBuf,
    license_url: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut manifest_url = None;
    let mut output = None;
    let mut license_url = None;
    let mut it = std::env::args().skip(1);

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--license-url" => {
                license_url = Some(it.next().ok_or("missing value for --license-url")?);
            }
            "-h" | "--help" => return Err(usage()),
            other if manifest_url.is_none() => manifest_url = Some(other.to_owned()),
            other if output.is_none() => output = Some(PathBuf::from(other)),
            other => return Err(format!("unknown argument: {other}\n\n{}", usage())),
        }
    }

    Ok(Args {
        manifest_url: manifest_url.ok_or_else(usage)?,
        output: output.ok_or_else(usage)?,
        license_url,
    })
}

fn usage() -> String {
    "usage: cargo run --example write_stream -- <manifest-url> <output-file> [--license-url <url>]"
        .to_owned()
}

fn is_fmp4_track(track: &PlayerTrackOutput) -> bool {
    matches!(
        track.info.mime_type.as_deref(),
        Some("video/mp4") | Some("audio/mp4")
    )
}

async fn collect_track_fmp4(
    mut rx: broadcast::Receiver<PlayerEvent>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut buf = Vec::new();
    loop {
        match rx.recv().await {
            Ok(PlayerEvent::Init(data)) | Ok(PlayerEvent::Segment { data, .. }) => {
                buf.extend_from_slice(&data);
            }
            Ok(PlayerEvent::End | PlayerEvent::PlaybackEnded) => break,
            Ok(PlayerEvent::Error(err)) => {
                return Err(Box::new(std::io::Error::other(err.0)));
            }
            Ok(_) => {}
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                return Err(format!(
                    "track event receiver lagged and dropped {skipped} fragments; \
                     subscribe before playback starts or reduce download rate"
                )
                .into());
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    Ok(buf)
}

async fn remux_to_mp4(
    track_buffers: &[Vec<u8>],
    output: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir =
        std::env::temp_dir().join(format!("dashplayrs-write-stream-{}", std::process::id()));
    tokio::fs::create_dir_all(&temp_dir).await?;

    let mut inputs = Vec::new();
    for (index, buf) in track_buffers.iter().enumerate() {
        if buf.is_empty() {
            continue;
        }
        let path = temp_dir.join(format!("track-{index}.mp4"));
        tokio::fs::write(&path, buf).await?;
        inputs.push(path);
    }

    if inputs.is_empty() {
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        return Err("no track data to remux".into());
    }

    let mut command = Command::new("ffmpeg");
    command
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error");
    for input in &inputs {
        command.arg("-i").arg(input);
    }
    command
        .arg("-c")
        .arg("copy")
        .arg("-movflags")
        .arg("+faststart")
        .arg(output);

    let status = command.status().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            std::io::Error::other("ffmpeg not found; install ffmpeg and ensure it is on PATH")
        } else {
            err
        }
    })?;

    let _ = tokio::fs::remove_dir_all(&temp_dir).await;

    if !status.success() {
        return Err(format!("ffmpeg remux failed with status {status}").into());
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args =
        parse_args().map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    let player = dashplayrs::Player::new(&args.manifest_url, args.license_url.as_deref())?;
    let outputs = player.start_tracks().await?;

    let collectors = outputs
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, track)| is_fmp4_track(track))
        .map(|(index, _)| {
            let rx = outputs
                .subscribe(index)
                .expect("track index validated above");
            collect_track_fmp4(rx)
        })
        .collect::<Vec<_>>();

    let (track_buffers, join_result) =
        tokio::join!(futures_util::future::try_join_all(collectors), outputs.join,);
    let track_buffers = track_buffers?;
    join_result??;

    remux_to_mp4(&track_buffers, &args.output).await?;

    let bytes_written = tokio::fs::metadata(&args.output).await?.len();
    println!("Wrote {bytes_written} bytes to {}", args.output.display());
    Ok(())
}
