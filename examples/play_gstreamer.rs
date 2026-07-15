//! Play a DASH stream with GStreamer.
//!
//! `dashplayrs` fetches and (when needed) decrypts ISOBMFF fragments; GStreamer demuxes,
//! decodes, and renders via `appsrc` → `decodebin` → auto sinks.
//!
//! Usage:
//! ```text
//! cargo run --example play_gstreamer --features example-gstreamer -- \
//!   https://dash.akamaized.net/akamai/bbb_30fps/bbb_30fps.mpd
//! ```
//!
//! For DRM-protected streams whose MPD does not include a license URL:
//! ```text
//! cargo run --example play_gstreamer --features example-gstreamer -- \
//!   https://example.com/manifest.mpd --license-url https://license.example/wv
//! ```
//!
//! Requires system GStreamer (≥ 1.14) with the usual base/good plugin set
//! (`appsrc`, `decodebin`, `qtdemux`, `autovideosink`, `autoaudiosink`).

use bytes::Bytes;
use dashplayrs::{PlayerEvent, TrackInfo, TrackKind};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use tokio::sync::broadcast;

#[derive(Debug)]
struct Args {
    manifest_url: String,
    license_url: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut manifest_url = None;
    let mut license_url = None;
    let mut it = std::env::args().skip(1);

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--license-url" => {
                license_url = Some(it.next().ok_or("missing value for --license-url")?);
            }
            "-h" | "--help" => return Err(usage()),
            other if manifest_url.is_none() => manifest_url = Some(other.to_owned()),
            other => return Err(format!("unknown argument: {other}\n\n{}", usage())),
        }
    }

    Ok(Args {
        manifest_url: manifest_url.ok_or_else(usage)?,
        license_url,
    })
}

fn usage() -> String {
    "usage: cargo run --example play_gstreamer --features example-gstreamer -- \
     <manifest-url> [--license-url <url>]"
        .to_owned()
}

fn is_playable_fmp4(info: &TrackInfo) -> bool {
    matches!(info.kind, TrackKind::Audio | TrackKind::Video)
        && matches!(
            info.mime_type.as_deref(),
            Some("video/mp4") | Some("audio/mp4")
        )
}

fn push_bytes(appsrc: &gst_app::AppSrc, data: &Bytes) -> Result<(), gst::FlowError> {
    let mut buffer = gst::Buffer::with_size(data.len()).map_err(|_| gst::FlowError::Error)?;
    {
        let buffer = buffer.get_mut().ok_or(gst::FlowError::Error)?;
        let mut map = buffer.map_writable().map_err(|_| gst::FlowError::Error)?;
        map.copy_from_slice(data);
    }
    appsrc.push_buffer(buffer).map(|_| ())
}

fn link_decodebin_pad(
    pipeline: &gst::Pipeline,
    pad: &gst::Pad,
    track_index: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(caps) = pad.current_caps() else {
        return Ok(());
    };
    let Some(structure) = caps.structure(0) else {
        return Ok(());
    };
    let media_type = structure.name();

    if media_type.starts_with("video/") {
        let convert = gst::ElementFactory::make("videoconvert")
            .name(format!("video-convert-{track_index}"))
            .build()?;
        let sink = gst::ElementFactory::make("autovideosink")
            .name(format!("video-sink-{track_index}"))
            .build()?;
        sink.set_property("sync", true);
        pipeline.add_many([&convert, &sink])?;
        convert.link(&sink)?;
        let sink_pad = convert
            .static_pad("sink")
            .ok_or("missing videoconvert sink pad")?;
        pad.link(&sink_pad)?;
        convert.sync_state_with_parent()?;
        sink.sync_state_with_parent()?;
    } else if media_type.starts_with("audio/") {
        let convert = gst::ElementFactory::make("audioconvert")
            .name(format!("audio-convert-{track_index}"))
            .build()?;
        let resample = gst::ElementFactory::make("audioresample")
            .name(format!("audio-resample-{track_index}"))
            .build()?;
        let sink = gst::ElementFactory::make("autoaudiosink")
            .name(format!("audio-sink-{track_index}"))
            .build()?;
        sink.set_property("sync", true);
        pipeline.add_many([&convert, &resample, &sink])?;
        gst::Element::link_many([&convert, &resample, &sink])?;
        let sink_pad = convert
            .static_pad("sink")
            .ok_or("missing audioconvert sink pad")?;
        pad.link(&sink_pad)?;
        convert.sync_state_with_parent()?;
        resample.sync_state_with_parent()?;
        sink.sync_state_with_parent()?;
    }

    Ok(())
}

fn build_track_branch(
    pipeline: &gst::Pipeline,
    track_index: usize,
    info: &TrackInfo,
) -> Result<gst_app::AppSrc, Box<dyn std::error::Error>> {
    let appsrc = gst::ElementFactory::make("appsrc")
        .name(format!("src-{track_index}"))
        .build()?
        .downcast::<gst_app::AppSrc>()
        .map_err(|_| "appsrc element is not an AppSrc")?;

    appsrc.set_format(gst::Format::Bytes);
    appsrc.set_is_live(true);
    appsrc.set_stream_type(gst_app::AppStreamType::Stream);
    appsrc.set_block(true);
    appsrc.set_max_bytes(8 * 1024 * 1024);
    appsrc.set_caps(Some(
        &gst::Caps::builder("video/quicktime")
            .field("variant", "iso-fragmented")
            .build(),
    ));

    let queue = gst::ElementFactory::make("queue")
        .name(format!("queue-{track_index}"))
        .build()?;
    queue.set_property("max-size-buffers", 0u32);
    queue.set_property("max-size-bytes", 0u32);
    queue.set_property("max-size-time", 0u64);

    let decodebin = gst::ElementFactory::make("decodebin")
        .name(format!("decode-{track_index}"))
        .build()?;

    pipeline.add_many([appsrc.upcast_ref(), &queue, &decodebin])?;
    gst::Element::link_many([appsrc.upcast_ref(), &queue, &decodebin])?;

    let pipeline_weak = pipeline.downgrade();
    let kind = info.kind;
    decodebin.connect_pad_added(move |_decodebin, pad| {
        let Some(pipeline) = pipeline_weak.upgrade() else {
            return;
        };
        if let Err(err) = link_decodebin_pad(&pipeline, pad, track_index) {
            eprintln!("failed to link {kind:?} pad for track {track_index}: {err}");
        }
    });

    Ok(appsrc)
}

async fn feed_track(
    mut rx: broadcast::Receiver<PlayerEvent>,
    appsrc: gst_app::AppSrc,
    track_index: usize,
) -> Result<(), String> {
    loop {
        match rx.recv().await {
            Ok(PlayerEvent::Init(data)) | Ok(PlayerEvent::Segment { data, .. }) => {
                if let Err(err) = push_bytes(&appsrc, &data) {
                    if matches!(err, gst::FlowError::Flushing | gst::FlowError::Eos) {
                        break;
                    }
                    return Err(format!("appsrc push failed for track {track_index}: {err}"));
                }
            }
            Ok(PlayerEvent::End | PlayerEvent::PlaybackEnded) => {
                let _ = appsrc.end_of_stream();
                break;
            }
            Ok(PlayerEvent::Error(err)) => {
                let _ = appsrc.end_of_stream();
                return Err(err.0);
            }
            Ok(PlayerEvent::BitrateChanged { to_bitrate_bps, .. }) => {
                eprintln!("track {track_index}: switched to {to_bitrate_bps} bps");
            }
            Ok(_) => {}
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                return Err(format!(
                    "track {track_index} event receiver lagged and dropped {skipped} fragments; \
                     subscribe before playback starts or reduce download rate"
                ));
            }
            Err(broadcast::error::RecvError::Closed) => {
                let _ = appsrc.end_of_stream();
                break;
            }
        }
    }
    Ok(())
}

fn bus_messages(bus: gst::Bus) -> Result<(), String> {
    for msg in bus.iter_timed(gst::ClockTime::NONE) {
        use gst::MessageView;
        match msg.view() {
            MessageView::Eos(..) => break,
            MessageView::Error(err) => {
                let src = msg
                    .src()
                    .map(|s| s.path_string())
                    .unwrap_or_else(|| "<unknown>".into());
                return Err(format!(
                    "GStreamer error from {src}: {} ({})",
                    err.error(),
                    err.debug().unwrap_or_default()
                ));
            }
            MessageView::Warning(warn) => {
                eprintln!(
                    "GStreamer warning: {} ({})",
                    warn.error(),
                    warn.debug().unwrap_or_default()
                );
            }
            _ => {}
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Video sinks on macOS need an NSApplication on the main thread.
    #[cfg(target_os = "macos")]
    {
        gst::macos_main(run)
    }
    #[cfg(not(target_os = "macos"))]
    {
        run()
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    let args =
        parse_args().map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    gst::init()?;

    let player = dashplayrs::Player::new(&args.manifest_url, args.license_url.as_deref())?;
    let mut outputs = player.start_tracks().await?;

    let pipeline = gst::Pipeline::with_name("dashplayrs-play");
    let mut feeders = Vec::new();
    let tracks = std::mem::take(&mut outputs.tracks);

    // Use the receivers created at `start_tracks` so Init/Segment events are not missed.
    for track in tracks {
        let info = track.info();
        if !is_playable_fmp4(&info) {
            continue;
        }
        let track_index = track.track_index;
        let appsrc = build_track_branch(&pipeline, track_index, &info)?;
        let rx = track.into_receiver();
        eprintln!(
            "playing track {track_index}: {:?} mime={:?}",
            info.kind, info.mime_type
        );
        feeders.push(tokio::spawn(feed_track(rx, appsrc, track_index)));
    }

    if feeders.is_empty() {
        outputs.stop()?;
        let _ = outputs.join.await;
        return Err("no playable fMP4 audio/video tracks found".into());
    }

    let bus = pipeline.bus().ok_or("pipeline has no bus")?;
    pipeline.set_state(gst::State::Playing)?;

    let bus_task = tokio::task::spawn_blocking(move || bus_messages(bus));
    let join_player = outputs.join;

    let (feed_results, bus_result, player_result) = tokio::join!(
        futures_util::future::try_join_all(feeders),
        bus_task,
        join_player,
    );

    let _ = pipeline.set_state(gst::State::Null);

    for result in feed_results? {
        result?;
    }
    bus_result??;
    player_result??;

    Ok(())
}
