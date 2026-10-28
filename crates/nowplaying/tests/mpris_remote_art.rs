//! A fake MPRIS player whose `mpris:artUrl` is an `http://` URL served locally.
//!
//! Needs a session D-Bus, so it is ignored by default. Run it with:
//! `dbus-run-session -- cargo test -p nowplaying --test mpris_remote_art -- --ignored`
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::time::Duration;

use zbus::zvariant::{OwnedValue, Value};

struct FakePlayer {
    art_url: String,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl FakePlayer {
    #[zbus(property)]
    fn playback_status(&self) -> String {
        "Playing".into()
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let mut m = HashMap::new();
        let mut put = |k: &str, v: Value<'_>| {
            m.insert(k.to_string(), v.try_to_owned().unwrap());
        };
        put("xesam:title", Value::from("Remote cover"));
        put("mpris:artUrl", Value::from(self.art_url.as_str()));
        m
    }
}

#[tokio::test]
#[ignore = "needs a session D-Bus (run under dbus-run-session)"]
async fn fetches_http_artwork() {
    let cover = nowplaying::PLACEHOLDER_COVER_BYTES;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let art_url = format!("http://{}/cover.jpg", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                cover.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(cover);
        }
    });

    let _conn = zbus::connection::Builder::session()
        .unwrap()
        .name("org.mpris.MediaPlayer2.remoteart")
        .unwrap()
        .serve_at("/org/mpris/MediaPlayer2", FakePlayer { art_url })
        .unwrap()
        .build()
        .await
        .expect("register fake MPRIS player");

    let watcher = nowplaying::NowPlayingWatcher::start();
    let mut rx = watcher.subscribe();
    let artwork = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            rx.changed().await.unwrap();
            if let Some(artwork) = rx.borrow_and_update().artwork.clone() {
                return artwork;
            }
        }
    })
    .await
    .expect("watcher publishes the downloaded artwork");
    assert_eq!(artwork.mime, "image/jpeg");
    assert_eq!(artwork.bytes, cover);
}
