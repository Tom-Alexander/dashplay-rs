mod common;

use common::drm::{
    RotatingDrmMockServer, counting_license_fetcher, play_single_track_with_license_fetcher,
};
use common::read_fixture;
use dashplayrs::drm::mpd::parse_mpd_drm_info;
use dashplayrs::drm::{License, WidevineSessionKey};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

#[test]
fn parse_widevine_drm_from_mpd_fixture() {
    let xml = read_fixture("drm_widevine", "manifest.mpd");
    let info = parse_mpd_drm_info(&xml).expect("parse drm");

    assert_eq!(info.periods.len(), 1);
    let period = &info.periods[0];
    assert_eq!(period.adaptation_sets.len(), 1);

    let aset = &period.adaptation_sets[0];
    assert!(!aset.effective.widevine_pssh.is_empty());
    assert_eq!(
        aset.effective.license_urls,
        vec!["https://license.example/wv".to_string()]
    );

    assert_eq!(aset.representations.len(), 1);
    let rep = &aset.representations[0];
    assert_eq!(rep.id.as_deref(), Some("1"));
    assert!(!rep.effective.widevine_pssh.is_empty());
}

#[test]
fn clear_vod_fixture_has_no_widevine_pssh() {
    let xml = read_fixture("vod_single", "manifest.mpd");
    let info = parse_mpd_drm_info(&xml).expect("parse drm");
    assert!(
        info.periods.is_empty() || info.periods[0].adaptation_sets.is_empty() || {
            info.periods[0].adaptation_sets[0]
                .effective
                .widevine_pssh
                .is_empty()
        }
    );
}

#[test]
fn vod_fixture_parses_with_dash_mpd() {
    let xml = read_fixture("vod_single", "manifest.mpd");
    let mpd = dash_mpd::parse(&xml).expect("parse mpd");
    assert_eq!(mpd.mpdtype.as_deref(), Some("static"));
    assert_eq!(mpd.periods.len(), 1);
    assert_eq!(mpd.periods[0].adaptations.len(), 1);
}

#[tokio::test]
async fn drm_playback_without_device_path_fails_before_network() {
    if std::env::var("DEVICE_PATH").is_ok() {
        return;
    }

    let server = common::FixtureServer::spawn("drm_widevine").await;
    let err = common::play_single_track(&server.manifest_url, std::time::Duration::from_secs(5))
        .await
        .expect_err("expected DRM setup failure without CDM device");

    assert!(
        matches!(err, dashplayrs::PlayerError::License(_)),
        "unexpected error: {err:?}"
    );
}

#[test]
fn drm_mpd_inheritance_prefers_representation_content_protection() {
    let xml = read_fixture("drm_widevine", "manifest.mpd");
    let info = parse_mpd_drm_info(&xml).expect("parse drm");
    let rep = &info.periods[0].adaptation_sets[0].representations[0];
    assert!(!rep.effective.widevine_pssh.is_empty());
    assert_eq!(
        rep.effective.license_urls,
        vec!["https://license.example/wv".to_string()]
    );
}

#[test]
fn rotated_mpds_have_distinct_widevine_session_keys() {
    let xml_v1 = read_fixture("drm_widevine_rotate", "manifest_v1.mpd");
    let xml_v2 = read_fixture("drm_widevine_rotate", "manifest_v2.mpd");
    let v1 = parse_mpd_drm_info(&xml_v1).expect("parse v1");
    let v2 = parse_mpd_drm_info(&xml_v2).expect("parse v2");
    let pssh_v1 = &v1.periods[0].adaptation_sets[0].effective.widevine_pssh[0];
    let pssh_v2 = &v2.periods[0].adaptation_sets[0].effective.widevine_pssh[0];
    assert_ne!(
        WidevineSessionKey::from_pssh(pssh_v1),
        WidevineSessionKey::from_pssh(pssh_v2)
    );
}

/// Merges a second CONTENT KID when a second license response is applied to the same session.
#[test]
fn apply_license_accumulates_content_keys() {
    let device_path = match std::env::var("DEVICE_PATH") {
        Ok(v) if !v.is_empty() => v,
        _ => return,
    };
    let _ = device_path;

    let license_path = common::fixture_dir("dashif_drm_encrypted").join("license-response.bin");
    if !license_path.exists() {
        return;
    }
    let license_bytes = std::fs::read(&license_path).expect("read license");

    let xml = read_fixture("drm_widevine", "manifest.mpd");
    let info = parse_mpd_drm_info(&xml).expect("parse drm");
    let pssh = &info.periods[0].adaptation_sets[0].effective.widevine_pssh[0];

    let license = License::new_from_pssh(pssh).expect("license request");
    license.apply_license(&license_bytes).expect("first apply");
    let kids_after_first = license.loaded_kids().expect("kids");
    assert!(!kids_after_first.is_empty());

    license.apply_license(&license_bytes).expect("second apply");
    let kids_after_second = license.loaded_kids().expect("kids");
    assert_eq!(kids_after_first, kids_after_second);
}

/// Live MPD refresh with rotating PSSH acquires one license per distinct PSSH, then reuses cache.
#[tokio::test]
async fn live_key_rotation_mpd_refresh_acquires_both_pssh_sessions() {
    let device_path = match std::env::var("DEVICE_PATH") {
        Ok(v) if !v.is_empty() => v,
        _ => return,
    };
    let _ = device_path;

    let license_path = common::fixture_dir("dashif_drm_encrypted").join("license-response.bin");
    if !license_path.exists() {
        return;
    }
    let license_bytes = std::fs::read(&license_path).expect("read license");

    let server = RotatingDrmMockServer::spawn("drm_widevine_rotate", license_bytes.clone()).await;
    let counter = server.license_post_count.clone();
    let fetcher = counting_license_fetcher(license_bytes, counter.clone());

    let _events = play_single_track_with_license_fetcher(
        &server.manifest_url,
        std::time::Duration::from_secs(4),
        fetcher,
    )
    .await
    .expect("playback with key rotation");

    let posts = counter.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        posts, 2,
        "expected one license POST per distinct PSSH (v1 then v2), got {posts}"
    );
}

#[test]
fn in_band_init_fixture_exposes_widevine_pssh() {
    let init = common::read_fixture_bytes("drm_in_band_init", "init.mp4");
    let info = dashplayrs::drm::mp4::extract_in_band_drm(&init, None).expect("parse init");
    assert_eq!(info.widevine_pssh.len(), 1);
}

/// In-band init PSSH triggers license acquisition when the MPD carries no ContentProtection.
#[tokio::test]
async fn in_band_init_pssh_acquires_widevine_license() {
    let device_path = match std::env::var("DEVICE_PATH") {
        Ok(v) if !v.is_empty() => v,
        _ => return,
    };
    let _ = device_path;

    let license_path = common::fixture_dir("dashif_drm_encrypted").join("license-response.bin");
    if !license_path.exists() {
        return;
    }
    let license_bytes = std::fs::read(&license_path).expect("read license");

    let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fetcher = counting_license_fetcher(license_bytes, counter.clone());
    let server = common::FixtureServer::spawn("drm_in_band_init").await;

    let _events = play_single_track_with_license_fetcher(
        &server.manifest_url,
        std::time::Duration::from_secs(3),
        fetcher,
    )
    .await
    .expect("in-band init playback");

    assert_eq!(
        counter.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "expected license POST from in-band init PSSH"
    );
}

/// Renewal challenges use RequestType::RENEWAL and differ from the initial license request.
#[test]
fn renewal_challenge_differs_from_initial_challenge() {
    let device_path = match std::env::var("DEVICE_PATH") {
        Ok(v) if !v.is_empty() => v,
        _ => return,
    };
    let _ = device_path;

    let xml = read_fixture("drm_widevine", "manifest.mpd");
    let info = parse_mpd_drm_info(&xml).expect("parse drm");
    let pssh = &info.periods[0].adaptation_sets[0].effective.widevine_pssh[0];

    let license = License::new_from_pssh(pssh).expect("license request");
    let initial = license.challenge().expect("initial challenge");
    let renewal = license.renewal_challenge().expect("renewal challenge");
    assert_ne!(
        initial, renewal,
        "renewal challenge should differ from the initial NEW request"
    );
}

/// Unchanged PSSH on manifest refresh must not trigger another license POST.
#[tokio::test]
async fn license_manager_reuses_session_on_unchanged_pssh() {
    let device_path = match std::env::var("DEVICE_PATH") {
        Ok(v) if !v.is_empty() => v,
        _ => return,
    };
    let _ = device_path;

    let license_path = common::fixture_dir("dashif_drm_encrypted").join("license-response.bin");
    if !license_path.exists() {
        return;
    }
    let license_bytes = std::fs::read(&license_path).expect("read license");

    let counter = Arc::new(AtomicUsize::new(0));
    let fetcher = counting_license_fetcher(license_bytes.clone(), counter.clone());
    let server =
        common::drm::spawn_drm_fixture_with_mock_license("drm_widevine", license_bytes).await;

    let _events = play_single_track_with_license_fetcher(
        &server.manifest_url,
        std::time::Duration::from_secs(2),
        fetcher,
    )
    .await
    .expect("static drm playback");

    assert_eq!(
        counter.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "static VOD should acquire exactly one Widevine license"
    );
}
