pub mod client;

use std::sync::Arc;

use color_eyre::eyre::Result;
use futures::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};

use self::client::{PcoClient, PcoObject};

pub const PCO_CONCURRENCY: usize = 5;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PcoEventWithRequestedResources {
  pub event:               PcoObject,
  pub instances:           Vec<PcoObject>,
  pub requested_resources: Vec<PcoObject>,
}

#[tracing::instrument(skip(client))]
pub async fn find_relevant_events(
  client: Arc<PcoClient>,
) -> Result<Vec<PcoEventWithRequestedResources>> {
  let start_date = chrono::Utc::now();
  let end_date = start_date + chrono::Duration::weeks(8);

  let event_instances = client
    .fetch_all_event_instances_in_range(start_date, end_date)
    .await?
    .try_collect::<Vec<_>>()
    .await?;

  let event_id_event_instance_map = Arc::new(
    event_instances
      .iter()
      .filter_map(|ei| {
        ei.relationships
          .as_ref()
          .and_then(|r| r.get("event"))
          .and_then(|er| er.get("data"))
          .and_then(|erd| erd.get("id"))
          .and_then(|id| id.as_str())
          .map(|id| id.to_string())
          .map(|id| (id, ei.clone()))
      })
      .fold(std::collections::HashMap::new(), |mut map, (id, ei)| {
        map.entry(id).or_insert_with(Vec::new).push(ei);
        map
      }),
  );

  futures::stream::iter(event_id_event_instance_map.keys())
    // fetch the event for each event ID
    // ---
    // the client is cloned twice because we need it later, and the async
    // block needs to own the client, but the closure is FnMut so it
    // can't give away ownership.
    .then({
      let client = client.clone();
      move |id| {
        let client = client.clone();
        async move { client.fetch_event(id.to_string()).await }
      }
    })
    // fetch the resource requests for each event
    .map_ok({
      let event_id_event_instance_map = event_id_event_instance_map.clone();
      move |event| {
        let client = client.clone();
        let event_id_event_instance_map = event_id_event_instance_map.clone();
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
            event:               event.clone(),
            instances:           event_id_event_instance_map
              .get(&event.id)
              .cloned()
              .unwrap_or_default(),
            requested_resources: resources,
          })
        }
      }
    })
    .try_buffer_unordered(PCO_CONCURRENCY)
    .try_collect()
    .await
}
