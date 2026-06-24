//! M5: a raw HEVC camera segment is transcoded on demand to browser-playable
//! H.264 (MPEG-TS) and cached. Generates a tiny HEVC with ffmpeg, stores it as a
//! camera blob, and asserts ensure_transcode produces H.264.

use std::process::Command;

use homeconnect::config::Config;

/// Make a short raw-HEVC clip with ffmpeg (testsrc), return its bytes.
fn make_hevc(dir: &std::path::Path) -> Vec<u8> {
    let out = dir.join("src.hevc");
    let ok = Command::new("ffmpeg")
        .args([
            "-nostdin", "-y", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=20:duration=1",
            "-c:v", "libx265", "-x265-params", "log-level=none", "-f", "hevc",
        ])
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("run ffmpeg")
        .success();
    assert!(ok, "ffmpeg failed to make hevc");
    std::fs::read(&out).unwrap()
}

/// Probe the first video stream's codec name.
fn codec_of(path: &std::path::Path) -> String {
    let out = Command::new("ffprobe")
        .args([
            "-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name",
            "-of", "default=nw=1:nk=1",
        ])
        .arg(path)
        .output()
        .expect("ffprobe");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

#[tokio::test]
async fn transcodes_hevc_to_h264() {
    let tmp = tempfile::tempdir().unwrap();
    let hevc = make_hevc(tmp.path());
    assert!(!hevc.is_empty());

    let mut config = Config::from_env();
    config.data_dir = tmp.path().join("data");
    let state = homeconnect::build_state(config).await.unwrap();

    let (dongle, ts, seg) = ("dongle0", "00000001--abc", 0i64);
    // Store the HEVC as the segment's fcamera blob.
    let key = homeconnect::storage::blob_key(dongle, ts, seg, "fcamera.hevc");
    state.blobs.put(&key, &hevc).await.unwrap();

    // Transcode.
    let out = homeconnect::transcode::ensure_transcode(&state, dongle, ts, seg, "fcamera")
        .await
        .expect("transcode");
    assert!(out.exists(), "cached .ts produced");
    assert_eq!(codec_of(&out), "h264", "output is H.264");

    // Second call hits the cache (same path, already present).
    let out2 = homeconnect::transcode::ensure_transcode(&state, dongle, ts, seg, "fcamera")
        .await
        .unwrap();
    assert_eq!(out, out2);

    // Duration probe works.
    let dur = homeconnect::transcode::probe_duration(&out).await;
    assert!(dur.map(|d| d > 0.0).unwrap_or(false), "probed positive duration");
}
