#![cfg(feature = "ffmpeg")]

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use throcc_client_core::call::Call;
use throcc_client_core::client::{Client, ClientApi, UiCommand};
use throcc_client_core::media::capture::{CaptureConfig, TestPattern};
use throcc_client_core::media::share::ShareSender;
use throcc_client_core::state::{ConnectionStatus, State, TrackHealth};
use throcc_proto::ids::UserId;
use throcc_proto::messages::{Codec, Request, Response, ShareConfig};
use tokio::runtime::Runtime;

use common::*;

const SHARE: ShareConfig = share_1080p60(Codec::H264, 8000);
/// A link several times faster than the test pattern needs.
const GOOD_LINK_KBPS: u64 = 4000;

/// A full client watching a test-pattern share sent by a bare call.
struct Watching {
    viewer: Client,
    sharer: UserId,
    _sender: Arc<ShareSender>,
    _call: Call,
    _viewer_dir: TempDir,
}

impl Watching {
    /// Has a sharer reaching the server at `sharer_addr` share a test pattern, and a viewer
    /// reaching it at `viewer_addr` watch it. `None`, reported as a skip, without an encoder.
    fn start(
        runtime: &Runtime,
        server: &ProcessLikeServer,
        sharer_addr: &str,
        viewer_addr: &str,
    ) -> Option<Self> {
        let (mut call, room, tracks) = runtime.block_on(async {
            let mut sharer = enroll_at(sharer_addr, &server.admin_invite()).await;
            let room = create_room(&mut sharer, "General").await;
            let call = Call::start(sharer.link, &sharer.keystore).unwrap();
            let (placement, _) = call.join(Some(room), None).await.unwrap();
            (call, room, placement.tracks.unwrap().shares[0])
        });
        let sharer = call.me();

        let viewer_dir = tempfile::tempdir().unwrap();
        let invite = server.admin_invite();
        let viewer = client_as(
            viewer_dir.path(),
            viewer_addr,
            Some(&invite),
            named("Viewer"),
        );
        wait_for_state(&viewer, "connected", |s| {
            s.connection == ConnectionStatus::Connected
        });
        viewer.command(UiCommand::Join(Some(room)));
        wait_for_state(&viewer, "the sharer in the room", |s| {
            s.person(sharer).is_some_and(|p| p.room == Some(room))
        });

        let sender = runtime.block_on(async {
            let capture = Box::new(TestPattern::new(
                CaptureConfig::for_share(&SHARE),
                Instant::now(),
            ));
            let sender = match ShareSender::start(call.engine.clone(), tracks, SHARE, capture) {
                Ok(sender) => Arc::new(sender),
                Err(e) => {
                    skip(format_args!("no encoder: {e}"));
                    return None;
                }
            };
            let reply = call.request(Request::SetMedia(sharing(sender.config)));
            assert_eq!(reply.await.unwrap(), Response::Ok);
            forward_keyframe_requests(&mut call, sender.clone());
            Some(sender)
        })?;
        wait_for_state(&viewer, "the share", |s| {
            s.person(sharer).is_some_and(|p| p.sharing.is_some())
        });
        viewer.command(UiCommand::Watch(sharer));
        Some(Self {
            viewer,
            sharer,
            _sender: sender,
            _call: call,
            _viewer_dir: viewer_dir,
        })
    }

    fn too_slow(&self) -> bool {
        self.viewer
            .state()
            .health
            .slow_shares
            .contains(&self.sharer)
    }

    /// The share's row in the viewer's health over the last refresh.
    fn screen_health(&self) -> Option<TrackHealth> {
        self.viewer
            .state()
            .health
            .receiving
            .iter()
            .find(|t| t.label.ends_with("screen"))
            .cloned()
    }

    /// Fails if the share shows as too slow at any point over `span`.
    fn assert_never_too_slow(&self, span: Duration) {
        let until = Instant::now() + span;
        while Instant::now() < until {
            assert!(!self.too_slow(), "the share showed as too slow");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_for_slow_label(&self, shown: bool, within: Duration) {
        wait_for_state_within(within, &self.viewer, "the slow label", |s| {
            s.health.slow_shares.contains(&self.sharer) == shown
        });
    }
}

fn wait_for_state(client: &Client, what: &str, ready: impl FnMut(&State) -> bool) {
    wait_for_state_within(Duration::from_secs(10), client, what, ready);
}

#[test]
#[ignore = "needs a hardware video encoder; virtual machines queue frames for seconds"]
fn a_share_loading_on_a_good_link_never_shows_as_too_slow() {
    let server = ProcessLikeServer::start_temp();
    let address = server.addr.to_string();
    let runtime = Runtime::new().unwrap();
    let relay = runtime.block_on(capped_relay(&address));
    relay.kbps.store(GOOD_LINK_KBPS, Ordering::Relaxed);
    let Some(watching) = Watching::start(&runtime, &server, &address, &relay.addr) else {
        return;
    };
    watching.assert_never_too_slow(Duration::from_secs(10));
    let arriving = watching.screen_health().map_or(0, |t| t.arriving_kbps);
    assert!(arriving > 0, "the share never arrived");
}

#[test]
#[ignore = "needs a hardware video encoder; virtual machines queue frames for seconds"]
fn a_share_shows_as_too_slow_while_our_link_cant_carry_it() {
    let server = ProcessLikeServer::start_temp();
    let address = server.addr.to_string();
    let runtime = Runtime::new().unwrap();
    let relay = runtime.block_on(capped_relay(&address));
    let Some(watching) = Watching::start(&runtime, &server, &address, &relay.addr) else {
        return;
    };
    watching.assert_never_too_slow(Duration::from_secs(6));

    let arriving = watching.screen_health().map_or(0, |t| t.arriving_kbps);
    assert!(arriving > 0, "the share never arrived");
    relay.kbps.store(u64::from(arriving / 3), Ordering::Relaxed);
    watching.wait_for_slow_label(true, Duration::from_secs(8));
    relay.kbps.store(0, Ordering::Relaxed);
    watching.wait_for_slow_label(false, Duration::from_secs(8));
    watching.assert_never_too_slow(Duration::from_secs(6));
}

#[test]
#[ignore = "needs a hardware video encoder; virtual machines queue frames for seconds"]
fn loss_on_the_sharers_upload_never_shows_the_share_as_too_slow() {
    let server = ProcessLikeServer::start_temp();
    let address = server.addr.to_string();
    let runtime = Runtime::new().unwrap();
    let relay = runtime.block_on(lossy_relay(&address, Toward::Server, 50));
    let Some(watching) = Watching::start(&runtime, &server, &relay.addr, &address) else {
        return;
    };
    relay.lossy.store(true, Ordering::Relaxed);
    watching.assert_never_too_slow(Duration::from_secs(10));
    assert!(
        relay.dropped.load(Ordering::Relaxed) > 0,
        "the relay dropped nothing"
    );
    let loss = watching.screen_health().and_then(|t| t.loss);
    assert!(
        loss.is_some_and(|loss| loss > 0.0),
        "the viewer saw no loss: {loss:?}"
    );
}
