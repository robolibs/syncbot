//! `did:key` authentication end to end (PLAN 2.2.1 step 3).
//!
//! A robot with an Ed25519 keypair cannot sign every request — the flat wire
//! carries one scalar in `key` — so it proves possession once against a fresh
//! challenge and carries a bearer token afterwards. Signing something fresh is
//! the point: a static signature would replay.

#![cfg(feature = "rest")]

use std::sync::Arc;

use datapod::{Geo, Point, Polygon};
use syncbot::wire::{ServeState, flat_challenge, flat_claim, flat_prove, flat_register};
use syncbot::{ClaimTargetKind, Coordinator, NUMERIC_ID_PROPERTY, WorkspaceIndex};
use zoneout::{Workspace, ZoneBuilder};

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Polygon {
    Polygon {
        vertices: vec![
            Point::new(x0, y0, 0.0),
            Point::new(x1, y0, 0.0),
            Point::new(x1, y1, 0.0),
            Point::new(x0, y1, 0.0),
        ],
    }
}

fn state() -> ServeState {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rect(0.0, 0.0, 1000.0, 1000.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root");
    for i in 0..3u64 {
        let x0 = 10.0 + i as f64 * 100.0;
        root.add_child(
            ZoneBuilder::new()
                .with_name(format!("z{i}"))
                .with_kind("zone")
                .with_boundary(rect(x0, 10.0, x0 + 50.0, 500.0))
                .with_datum(Geo::new(52.0, 5.0, 0.0))
                .with_property(NUMERIC_ID_PROPERTY, i.to_string())
                .with_property("traffic.policy", "exclusive")
                .build()
                .expect("zone"),
        )
        .expect("add");
    }
    let index = Arc::new(WorkspaceIndex::new(Arc::new(Workspace::new(root))));
    ServeState::new(Coordinator::with_index(index))
        .with_kdf_params(syncbot::core::key::insecure_test_cost())
}

/// A robot: an Ed25519 keypair and the `did:key` that names it.
struct Robot {
    did: String,
    secret: Vec<u8>,
}

impl Robot {
    fn new() -> Self {
        let (public, secret) = keylock::crypto::ed25519::keypair().expect("keypair");
        let mut key = [0u8; 32];
        key.copy_from_slice(&public);
        Self {
            did: authbox::did::key::encode_ed25519_did_key(key).expect("did:key"),
            secret,
        }
    }

    fn sign_hex(&self, nonce_hex: &str) -> String {
        let nonce = hex(nonce_hex);
        let signature =
            keylock::crypto::ed25519::sign_detached(&nonce, &self.secret).expect("sign");
        signature.iter().map(|b| format!("{b:02x}")).collect()
    }
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

/// The whole flow: challenge, sign, token, register, work.
#[test]
fn a_did_key_robot_proves_itself_and_then_works() {
    let state = state();
    let robot = Robot::new();

    // A fresh nonce, before the robot is registered.
    let challenge = flat_challenge(&state, "7").expect("challenge");
    assert_eq!(challenge.nonce.len(), 64, "32 bytes of nonce, hex encoded");

    // Signing it yields a bearer token.
    let proof =
        flat_prove(&state, "7", &robot.did, &robot.sign_hex(&challenge.nonce)).expect("prove");
    assert!(!proof.token.is_empty());

    // Registering with the token binds the identity the token proved.
    let key = format!("tok:{}", proof.token);
    assert_eq!(flat_register(&state, "7", &key, None).decision, 1);

    // And the token authenticates ordinary work.
    let claim = flat_claim(&state, ClaimTargetKind::Zone, &key, "7", &[0], None, None);
    assert_eq!((claim.decision, claim.reason), (1, 0));
}

/// The signature must be over *this* challenge. A stale one does not carry.
#[test]
fn a_signature_over_a_stale_challenge_is_refused() {
    let state = state();
    let robot = Robot::new();

    let first = flat_challenge(&state, "7").expect("challenge");
    let stale = robot.sign_hex(&first.nonce);
    // Asking again replaces the outstanding nonce.
    let _second = flat_challenge(&state, "7").expect("challenge");

    assert!(
        flat_prove(&state, "7", &robot.did, &stale).is_err(),
        "a signature over the previous challenge must not be accepted"
    );
}

/// A challenge is single use, so a captured proof cannot be replayed.
#[test]
fn a_challenge_cannot_be_answered_twice() {
    let state = state();
    let robot = Robot::new();

    let challenge = flat_challenge(&state, "7").expect("challenge");
    let signature = robot.sign_hex(&challenge.nonce);

    assert!(flat_prove(&state, "7", &robot.did, &signature).is_ok());
    assert!(
        flat_prove(&state, "7", &robot.did, &signature).is_err(),
        "replaying the same proof must not yield a second token"
    );
}

/// Holding the public key is not holding the private key.
#[test]
fn the_did_alone_proves_nothing() {
    let state = state();
    let robot = Robot::new();

    // Register robot 7 properly.
    let challenge = flat_challenge(&state, "7").expect("challenge");
    let proof =
        flat_prove(&state, "7", &robot.did, &robot.sign_hex(&challenge.nonce)).expect("prove");
    flat_register(&state, "7", &format!("tok:{}", proof.token), None);

    // An eavesdropper knows the DID — it is public — but cannot use it.
    let claim = flat_claim(
        &state,
        ClaimTargetKind::Zone,
        &robot.did,
        "7",
        &[1],
        None,
        None,
    );
    assert_eq!(
        claim.reason, 1,
        "presenting the public key must not authenticate"
    );

    // Nor can they answer a challenge without the private key.
    let _ = flat_challenge(&state, "7").expect("challenge");
    let forged = "00".repeat(64);
    assert!(flat_prove(&state, "7", &robot.did, &forged).is_err());
}

/// A second robot cannot prove *its own* key against someone else's id.
#[test]
fn a_different_key_cannot_claim_a_registered_id() {
    let state = state();
    let owner = Robot::new();
    let attacker = Robot::new();

    let challenge = flat_challenge(&state, "7").expect("challenge");
    let proof =
        flat_prove(&state, "7", &owner.did, &owner.sign_hex(&challenge.nonce)).expect("prove");
    flat_register(&state, "7", &format!("tok:{}", proof.token), None);

    // The attacker signs correctly — with the wrong identity for this id.
    let challenge = flat_challenge(&state, "7").expect("challenge");
    let signature = attacker.sign_hex(&challenge.nonce);
    assert!(
        flat_prove(&state, "7", &attacker.did, &signature).is_err(),
        "robot 7 is bound to the owner's key; another key must not prove it"
    );
}

/// A token belongs to the robot that earned it.
#[test]
fn a_token_does_not_work_for_another_robot() {
    let state = state();
    let seven = Robot::new();
    let eight = Robot::new();

    for (id, robot) in [("7", &seven), ("8", &eight)] {
        let challenge = flat_challenge(&state, id).expect("challenge");
        let proof =
            flat_prove(&state, id, &robot.did, &robot.sign_hex(&challenge.nonce)).expect("prove");
        flat_register(&state, id, &format!("tok:{}", proof.token), None);
    }

    // Robot 8 gets a token, then tries to act as robot 7 with it.
    let challenge = flat_challenge(&state, "8").expect("challenge");
    let proof =
        flat_prove(&state, "8", &eight.did, &eight.sign_hex(&challenge.nonce)).expect("prove");
    let claim = flat_claim(
        &state,
        ClaimTargetKind::Zone,
        &format!("tok:{}", proof.token),
        "7",
        &[2],
        None,
        None,
    );
    assert_eq!(claim.reason, 1, "a token is bound to one robot");
}

/// A password robot cannot be taken over by signing, and a did:key robot
/// cannot be taken over with a password.
#[test]
fn the_two_identity_kinds_do_not_cross_over() {
    let state = state();
    let robot = Robot::new();

    // Robot 7 registers with a password.
    assert_eq!(flat_register(&state, "7", "1234", None).decision, 1);

    // Signing proves nothing about a password-held id.
    let challenge = flat_challenge(&state, "7").expect("challenge");
    assert!(
        flat_prove(&state, "7", &robot.did, &robot.sign_hex(&challenge.nonce)).is_err(),
        "a signature must not take over an id held by a password"
    );

    // Robot 8 registers with did:key.
    let challenge = flat_challenge(&state, "8").expect("challenge");
    let proof =
        flat_prove(&state, "8", &robot.did, &robot.sign_hex(&challenge.nonce)).expect("prove");
    flat_register(&state, "8", &format!("tok:{}", proof.token), None);

    // No password opens it.
    for guess in ["1234", "0", "pass:anything"] {
        let claim = flat_claim(&state, ClaimTargetKind::Zone, guess, "8", &[0], None, None);
        assert_eq!(
            claim.reason, 1,
            "{guess} must not authenticate a did:key robot"
        );
    }
}

/// A public key is public, so naming one must not bind it. Otherwise anyone
/// could take an id the real holder then cannot register.
#[test]
fn an_identity_cannot_be_squatted_without_proving_it() {
    let state = state();
    let robot = Robot::new();

    // A squatter who has only seen the DID cannot bind it.
    let squat = flat_register(&state, "7", &robot.did, None);
    assert_eq!(
        (squat.decision, squat.reason),
        (0, 5),
        "a did:key must be proven before it can be registered"
    );

    // The real holder still gets its id, by proving possession.
    let challenge = flat_challenge(&state, "7").expect("challenge");
    let proof =
        flat_prove(&state, "7", &robot.did, &robot.sign_hex(&challenge.nonce)).expect("prove");
    assert_eq!(
        flat_register(&state, "7", &format!("tok:{}", proof.token), None).decision,
        1
    );
}
