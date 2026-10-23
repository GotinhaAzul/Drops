//! Poll geometry only when window blobs exist; no context capture or idle subprocesses.
use crate::{refresh_anchors, Shared};
use std::{collections::BTreeMap, time::Duration};

use tokio_util::sync::CancellationToken;

type Geometry = BTreeMap<String, ([i32; 2], [i32; 2])>;
pub async fn watch(app: Shared, stop: CancellationToken) {
    let mut ticks = tokio::time::interval(Duration::from_millis(200));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut socket = None;
    let mut previous = Geometry::new();
    loop {
        tokio::select! {_=stop.cancelled()=>break,_=ticks.tick()=>{}}
        let addresses: Vec<String> = app
            .lock()
            .await
            .snapshot
            .blobs
            .iter()
            .filter(|b| b.placement["anchor"]["kind"] == "window")
            .filter_map(|b| b.placement["anchor"]["address"].as_str().map(str::to_owned))
            .collect();
        if addresses.is_empty() {
            previous.clear();
            continue;
        }
        let result = tokio::select! {_=stop.cancelled()=>break,r=buddy_anchor::control::clients(&mut socket)=>r};
        let Ok(clients) = result else {
            continue;
        };
        let current: Geometry = clients
            .into_iter()
            .filter(|c| addresses.contains(&c.address))
            .map(|c| (c.address, (c.at, c.size)))
            .collect();
        if current != previous {
            let refreshed =
                tokio::select! {_=stop.cancelled()=>break,result=refresh_anchors(&app)=>result};
            if refreshed {
                previous = current;
            }
        }
    }
}
