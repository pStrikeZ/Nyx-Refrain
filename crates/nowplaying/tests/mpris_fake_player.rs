//! End-to-end check of the MPRIS backend against a fake player on a private session bus.
//!
//! Needs a session D-Bus, so it is ignored by default. Run it with:
//! `dbus-run-session -- cargo test -p nowplaying --test mpris_fake_player -- --ignored`
#![cfg(target_os = "linux")]

use nowplaying::MediaCommand;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::zvariant::{ObjectPath, OwnedValue, Value};

#[derive(Debug, PartialEq, Eq)]
enum Call {
    Play,
    Pause,
    PlayPause,
    Next,
    Previous,
    Position(String, i64),
}

struct PlayerState {
    playing: bool,
    can_seek: bool,
    duration: Option<i64>,
    track: Option<String>,
    calls: Vec<Call>,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            playing: true,
            can_seek: true,
            duration: Some(261_000_000),
            track: Some("/track/one".into()),
            calls: Vec::new(),
        }
    }
}

struct FakePlayer {
    art_url: String,
    title: String,
    state: Arc<Mutex<PlayerState>>,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl FakePlayer {
    #[zbus(property)]
    fn playback_status(&self) -> String {
        if self.state.lock().unwrap().playing {
            "Playing"
        } else {
            "Paused"
        }
        .into()
    }

    fn play(&self) {
        self.state.lock().unwrap().calls.push(Call::Play);
    }
    fn pause(&self) {
        self.state.lock().unwrap().calls.push(Call::Pause);
    }
    fn play_pause(&self) {
        self.state.lock().unwrap().calls.push(Call::PlayPause);
    }
    fn next(&self) {
        self.state.lock().unwrap().calls.push(Call::Next);
    }
    fn previous(&self) {
        self.state.lock().unwrap().calls.push(Call::Previous);
    }
    fn set_position(&self, track_id: ObjectPath<'_>, position: i64) {
        self.state
            .lock()
            .unwrap()
            .calls
            .push(Call::Position(track_id.to_string(), position));
    }

    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.state.lock().unwrap().can_seek
    }

    #[zbus(property)]
    fn position(&self) -> i64 {
        42_000_000
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let mut m = HashMap::new();
        let mut put = |k: &str, v: Value<'_>| {
            m.insert(k.to_string(), v.try_to_owned().unwrap());
        };
        put("xesam:title", Value::from(self.title.as_str()));
        put("xesam:artist", Value::from(vec!["YOASOBI", "Ayase"]));
        put("xesam:album", Value::from("THE BOOK"));
        let state = self.state.lock().unwrap();
        if let Some(length) = state.duration {
            put("mpris:length", Value::from(length));
        }
        if let Some(track) = &state.track {
            put(
                "mpris:trackid",
                Value::from(ObjectPath::try_from(track.as_str()).unwrap()),
            );
        }
        put("mpris:artUrl", Value::from(self.art_url.as_str()));
        m
    }
}

#[tokio::test]
#[ignore = "needs a session D-Bus (run under dbus-run-session)"]
async fn reads_metadata_from_mpris_player() {
    let dir = std::env::temp_dir().join(format!("nowplaying-mpris-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let art = dir.join("cover art.png");
    image::RgbImage::new(1, 1).save(&art).unwrap();
    let png = std::fs::read(&art).unwrap();
    let art_url = format!("file://{}", art.display()).replace(' ', "%20");

    let state = Arc::new(Mutex::new(PlayerState::default()));
    let conn = zbus::connection::Builder::session()
        .unwrap()
        .name("org.mpris.MediaPlayer2.fake")
        .unwrap()
        .serve_at(
            "/org/mpris/MediaPlayer2",
            FakePlayer {
                art_url,
                title: "夜に駆ける".into(),
                state: state.clone(),
            },
        )
        .unwrap()
        .build()
        .await
        .expect("register fake MPRIS player");

    let watcher = nowplaying::NowPlayingWatcher::start();
    let mut rx = watcher.subscribe();
    let np = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            rx.changed().await.unwrap();
            let np = rx.borrow_and_update().clone();
            if !np.is_empty() {
                return np;
            }
        }
    })
    .await
    .expect("watcher publishes the fake player's state");

    assert_eq!(np.title, "夜に駆ける");
    assert_eq!(np.artist, "YOASOBI, Ayase");
    assert_eq!(np.album, "THE BOOK");
    assert!(np.playing);
    assert_eq!(np.duration, Some(Duration::from_secs(261)));
    assert_eq!(np.position, Some(Duration::from_secs(42)));
    let artwork = np.artwork.expect("file:// artwork loaded");
    assert_eq!(artwork.mime, "image/png");
    assert_eq!(artwork.bytes, png);

    // A second player is present, but commands must follow the selected metadata session.
    let other_state = Arc::new(Mutex::new(PlayerState {
        playing: false,
        ..Default::default()
    }));
    let _other = zbus::connection::Builder::session()
        .unwrap()
        .name("org.mpris.MediaPlayer2.other")
        .unwrap()
        .serve_at(
            "/org/mpris/MediaPlayer2",
            FakePlayer {
                art_url: String::new(),
                title: "Other player".into(),
                state: other_state.clone(),
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    for command in [
        MediaCommand::Play,
        MediaCommand::Pause,
        MediaCommand::PlayPause,
        MediaCommand::Next,
        MediaCommand::Previous,
        MediaCommand::Seek(Duration::from_millis(12345)),
        MediaCommand::Seek(Duration::from_secs(999)),
    ] {
        assert!(watcher.control(command));
    }
    wait_for_calls(&state, 7).await;
    assert_eq!(
        state.lock().unwrap().calls,
        vec![
            Call::Play,
            Call::Pause,
            // PlayPause resolves from PlaybackStatus ("Playing") instead of the player's toggle.
            Call::Pause,
            Call::Next,
            Call::Previous,
            Call::Position("/track/one".into(), 12_345_000),
            Call::Position("/track/one".into(), 261_000_000)
        ]
    );
    assert!(other_state.lock().unwrap().calls.is_empty());

    // A following Next is a dispatch barrier: the unsupported Seek must be consumed first.
    state.lock().unwrap().can_seek = false;
    assert!(watcher.control(MediaCommand::Seek(Duration::from_secs(10))));
    assert!(watcher.control(MediaCommand::Next));
    wait_for_calls(&state, 8).await;
    {
        let mut state = state.lock().unwrap();
        assert_eq!(state.calls.last(), Some(&Call::Next));
        state.can_seek = true;
        state.duration = None;
    }
    assert!(watcher.control(MediaCommand::Seek(Duration::from_secs(10))));
    assert!(watcher.control(MediaCommand::Next));
    wait_for_calls(&state, 9).await;
    {
        let mut state = state.lock().unwrap();
        assert_eq!(state.calls.last(), Some(&Call::Next));
        state.duration = Some(261_000_000);
        state.track = None;
    }
    assert!(watcher.control(MediaCommand::Seek(Duration::from_secs(10))));
    assert!(watcher.control(MediaCommand::Next));
    wait_for_calls(&state, 10).await;
    assert_eq!(state.lock().unwrap().calls.last(), Some(&Call::Next));
    state.lock().unwrap().track = Some("/track/two".into());
    assert!(watcher.control(MediaCommand::Seek(Duration::from_secs(1))));
    wait_for_calls(&state, 11).await;
    assert_eq!(
        state.lock().unwrap().calls.last(),
        Some(&Call::Position("/track/two".into(), 1_000_000))
    );

    // Switching the metadata selection switches control as well.
    state.lock().unwrap().playing = false;
    other_state.lock().unwrap().playing = true;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            rx.changed().await.unwrap();
            if rx.borrow_and_update().title == "Other player" {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(watcher.control(MediaCommand::Pause));
    wait_for_calls(&other_state, 1).await;
    assert_eq!(other_state.lock().unwrap().calls, vec![Call::Pause]);
    assert_eq!(state.lock().unwrap().calls.len(), 11);
    drop(watcher);
    drop(conn);

    let _ = std::fs::remove_dir_all(&dir);
}

async fn wait_for_calls(state: &Mutex<PlayerState>, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if state.lock().unwrap().calls.len() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("MPRIS command dispatched");
}
