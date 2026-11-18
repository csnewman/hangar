//! Relaying through the app, against two servers of a test's own that are
//! linked yet cannot reach each other: HANGAR_RELAY_A and HANGAR_RELAY_B
//! each name one as url|token, A holding a pack named "carried" that B
//! does not. Without them the test passes untried.

use std::sync::Arc;
use std::time::{Duration, Instant};

use hangar_app::api::Client;
use hangar_app::relay::{PlaceInfo, Relay};

fn info(id: &str, c: &Client) -> PlaceInfo {
    let l = c.links().expect("the place's links");
    PlaceInfo {
        id: id.into(),
        name: id.into(),
        client: c.clone(),
        place_id: l.place.id.clone(),
        links: l
            .items
            .iter()
            .map(|x| (x.place.clone(), false, None))
            .collect(),
    }
}

#[test]
fn a_relayed_place_reaches_the_other_through_the_app() {
    let (Ok(a), Ok(b)) = (
        std::env::var("HANGAR_RELAY_A"),
        std::env::var("HANGAR_RELAY_B"),
    ) else {
        return;
    };
    let client = |v: &str| {
        let (url, token) = v.split_once('|').expect("url|token");
        Client::new(url, token)
    };
    let (ca, cb) = (client(&a), client(&b));
    let relay = Arc::new(Relay::open(None));
    relay.set_place("a", true);
    relay.set_on_expensive(true);
    relay.tick(&[info("a", &ca), info("b", &cb)]);
    let start = Instant::now();
    loop {
        let carried = cb
            .packs()
            .expect("b's packs")
            .iter()
            .any(|k| k.name == "carried");
        if carried {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "b never took a's pack"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(relay.relaying("a"), vec!["b".to_string()]);
    assert!(relay.carried("a") > 0, "what it carried is counted");
    relay.set_place("a", false);
}
