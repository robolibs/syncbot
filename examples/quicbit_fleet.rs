//! Publish timenav fleet events over quicbit and read them back.
//!
//! One process runs both ends for the demo; in production the
//! subscriber is a separate binary (dashboard, logger, another robot).
//! Same-host subscribers attach over iceoryx2 shared memory; off-host
//! ones would dial in over iroh — this code is identical either way.
//!
//! ```sh
//! cargo run --example quicbit_fleet --features quicbit
//! ```

use std::time::Duration;

use quicbit::Node;
use timenav::claim::{ClaimAccessMode, ClaimId, Lease, LeaseId, RobotId};
use timenav::wire::quicbit::{FleetPublisher, LeaseEvent, LeaseEventKind, TOPIC_LEASE};

fn main() -> quicbit::Result<()> {
    // Publisher side: the coordinator's identity.
    let mut fleet = FleetPublisher::new("timenav-core")?;

    // Subscriber side: a dashboard listening to the coordinator.
    let dash = Node::builder().identity("dashboard").no_relay().bind()?;
    let mut sub = dash.subscriber::<LeaseEvent>("timenav-core", TOPIC_LEASE)?;

    // Simulate two lease grants and one release.
    let lease_a = Lease {
        id: LeaseId::new(111),
        claim_id: ClaimId::new(11),
        robot_id: RobotId::new(1),
        access_mode: ClaimAccessMode::Exclusive,
        ..Lease::default()
    };
    let lease_b = Lease {
        id: LeaseId::new(222),
        claim_id: ClaimId::new(22),
        robot_id: RobotId::new(2),
        access_mode: ClaimAccessMode::Exclusive,
        ..Lease::default()
    };

    fleet.publish_lease(&lease_a, LeaseEventKind::Granted, 100, 0)?;
    fleet.publish_lease(&lease_b, LeaseEventKind::Granted, 101, 0)?;
    fleet.publish_lease(&lease_a, LeaseEventKind::Released, 100, 5)?;

    // Drain what the dashboard sees.
    let mut seen = 0;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while seen < 3 && std::time::Instant::now() < deadline {
        match sub.take()? {
            Some(sample) => {
                let e = sample.header();
                let kind = match e.kind {
                    0u64 => "GRANTED",
                    1 => "RELEASED",
                    2 => "EXPIRED",
                    3 => "REVOKED",
                    _ => "?",
                };
                println!(
                    "dashboard: {kind} robot={} lease={} zone#{} @tick {}",
                    e.robot_id, e.lease_id, e.zone_numeric_id, e.tick
                );
                seen += 1;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }

    println!("received {seen}/3 lease events");
    Ok(())
}
