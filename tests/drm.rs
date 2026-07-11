mod common;

use common::read_fixture;
use dashplayrs::drm::mpd::parse_mpd_drm_info;

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
