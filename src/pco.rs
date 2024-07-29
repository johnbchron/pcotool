pub mod client;

use std::sync::Arc;

use color_eyre::eyre::Result;
use futures::{future::ready, stream::BoxStream, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};

use self::client::{PcoClient, PcoObject};

pub const PCO_CONCURRENCY: usize = 5;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PcoEventWithRequestedResources {
  pub event:               PcoObject,
  pub requested_resources: Vec<PcoObject>,
}

#[tracing::instrument(skip(client))]
pub async fn find_relevant_events(
  client: Arc<PcoClient>,
) -> impl futures::Stream<Item = Result<PcoEventWithRequestedResources>> {
  let start_date = chrono::Utc::now();
  let end_date = start_date + chrono::Duration::weeks(8);

  let event_instance_stream = client
    .fetch_all_event_instances_in_range(start_date, end_date)
    .await;
  if let Err(e) = event_instance_stream {
    return Box::pin(futures::stream::once(ready(Err(e)))) as BoxStream<_>;
  }
  let event_instance_stream = event_instance_stream.unwrap();

  let stream = event_instance_stream
    // map event instances to their event IDs
    .map_ok(|ei| {
      let event_id = ei
        .relationships
        .as_ref()
        .and_then(|r| r.get("event"))
        .and_then(|er| er.get("data"))
        .and_then(|erd| erd.get("id"))
        .and_then(|id| id.as_str())
        .map(|id| id.to_string());
      if event_id.is_none() {
        tracing::warn!("failed to find event ID for event instance: {}", ei.id);
      }
      event_id
    })
    // filter out event instances without event IDs
    .try_filter_map(|v| futures::future::ready(Ok(v)))
    // use a hashset with scan to deduplicate event IDs
    .scan(std::collections::HashSet::new(), |set, v| match v {
      Ok(v) => {
        if set.contains(&v) {
          ready(Some(Ok(None)))
        } else {
          set.insert(v.clone());
          ready(Some(Ok(Some(v))))
        }
      }
      Err(e) => ready(Some(Err(e))),
    })
    // filter out the deduplicated values
    .try_filter_map(|v| ready(Ok(v)))
    // fetch the event for each event ID
    // ---
    // the client is cloned twice because we need it later, and the async block
    // needs to own the client, but the closure is FnMut so it can't give away
    //ownership.
    .and_then({
      let client = client.clone();
      move |id| {
        let client = client.clone();
        async move { client.fetch_event(id).await }
      }
    })
    // fetch the resource requests for each event
    .map_ok(move |event| {
      let client = client.clone();
      async move {
        let resource_requests = client
          .fetch_resource_requests_for_event(event.id.clone())
          .await?;
        // fetch the resources for each resource request
        let resources = futures::stream::iter(resource_requests)
          .map(|rr| {
            let resource_id = rr
              .relationships
              .as_ref()
              .and_then(|r| r.get("resource"))
              .and_then(|rr| rr.get("data"))
              .and_then(|d| d.get("id"))
              .and_then(|id| id.as_str())
              .map(|id| id.to_string());
            if resource_id.is_none() {
              tracing::warn!(
                "failed to find resource ID for resource request: {}",
                rr.id
              );
            }
            resource_id
          })
          .filter_map(futures::future::ready)
          .map(move |id| {
            let client = client.clone();
            async move { client.fetch_resource(id).await }
          })
          .buffer_unordered(PCO_CONCURRENCY)
          .try_collect::<Vec<PcoObject>>()
          .await?;
        Ok(PcoEventWithRequestedResources {
          event,
          requested_resources: resources,
        })
      }
    })
    .try_buffer_unordered(PCO_CONCURRENCY);

  // .map_ok(move |id| {
  //   let client = client.clone();
  //   async move { client.fetch_event(id).await }
  // })
  // // buffer the requests
  // .try_buffered(PCO_CONCURRENCY);

  Box::pin(stream) as BoxStream<_>
}
