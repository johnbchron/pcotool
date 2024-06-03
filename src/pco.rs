pub mod client;

use std::{ops::Deref, sync::Arc};

use color_eyre::eyre::{OptionExt, Report, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::{
  mpsc::{self, Sender},
  Semaphore,
};

use self::client::{PcoClient, PcoObject};
use crate::DateTimeUtc;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PcoInstancedEvent {
  pub event_id:          String,
  pub event_name:        String,
  pub starts_at:         DateTimeUtc,
  pub ends_at:           DateTimeUtc,
  pub event_url:         String,
  pub instance_id:       String,
  pub instance_url:      String,
  pub resource_requests: Vec<PcoResourceRequestWithResource>,
}

impl PcoInstancedEvent {
  pub fn from_pieces(
    event: PcoObject,
    instance: PcoObject,
    resource_requests: Vec<PcoResourceRequestWithResource>,
  ) -> Option<Self> {
    Some(Self {
      event_id: event.id,
      event_name: event
        .attributes
        .as_ref()?
        .get("name")?
        .as_str()?
        .to_string(),
      starts_at: instance
        .attributes
        .as_ref()?
        .get("starts_at")?
        .as_str()?
        .parse()
        .ok()?,
      ends_at: instance
        .attributes
        .as_ref()?
        .get("ends_at")?
        .as_str()?
        .parse()
        .ok()?,
      event_url: event
        .links
        .as_ref()?
        .get("html")?
        .as_ref()?
        .as_str()
        .to_string(),
      instance_id: instance.id,
      instance_url: instance
        .attributes
        .as_ref()?
        .get("church_center_url")?
        .as_str()?
        .to_string(),
      resource_requests,
    })
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum PcoApprovalStatus {
  Approved,
  Pending,
  Rejected,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PcoResourceRequest {
  pub id:              String,
  pub approval_status: PcoApprovalStatus,
  pub quantity:        i64,
  pub resource_id:     String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PcoResourceRequestWithResource {
  pub resource_request: PcoResourceRequest,
  pub resource_name:    String,
}

impl PcoResourceRequest {
  pub fn from_object(object: PcoObject) -> Option<Self> {
    let approval_status = match object
      .attributes
      .as_ref()?
      .get("approval_status")?
      .as_str()?
    {
      "A" => PcoApprovalStatus::Approved,
      "P" => PcoApprovalStatus::Pending,
      "R" => PcoApprovalStatus::Rejected,
      p => {
        tracing::error!(
          "got unexpected event_resource_request approval_status: {p}"
        );
        None?
      }
    };
    let resource_id = object
      .relationships
      .as_ref()?
      .get("resource")?
      .as_object()?
      .get("data")?
      .as_object()?
      .get("id")?
      .as_str()?
      .to_string();
    Some(PcoResourceRequest {
      id: object.id,
      approval_status,
      quantity: object.attributes.as_ref()?.get("quantity")?.as_i64()?,
      resource_id,
    })
  }

  pub async fn fetch_resource(
    self,
    client: impl Deref<Target = PcoClient>,
  ) -> Result<PcoResourceRequestWithResource> {
    let resource = client
      .fetch_resource(&self.resource_id)
      .await?
      .to_single()?;
    let resource_name = resource
      .attributes
      .as_ref()
      .ok_or_eyre("no attributes present in response")?
      .get("name")
      .ok_or_eyre("no name in attributes")?
      .as_str()
      .ok_or_eyre("name attribute is not string")?
      .to_string();

    Ok(PcoResourceRequestWithResource {
      resource_request: self,
      resource_name,
    })
  }
}

async fn handle_event_with_instances(
  progress: Arc<indicatif::ProgressBar>,
  semaphore: Arc<Semaphore>,
  client: Arc<PcoClient>,
  tx: Sender<PcoInstancedEvent>,
  event: PcoObject,
  instances: Vec<PcoObject>,
) -> Result<()> {
  // filter the instances based on time.
  // the `starts_at` attribute must be more recent than 1 week ago.
  let original_count = instances.len();
  let instances = instances
    .into_iter()
    .filter(|i| {
      let instance_starts_at = chrono::DateTime::<chrono::Utc>::from(
        chrono::DateTime::parse_from_rfc3339(
          i.attributes
            .as_ref()
            .unwrap()
            .get("starts_at")
            .unwrap()
            .as_str()
            .unwrap(),
        )
        .unwrap(),
      );
      instance_starts_at + chrono::Duration::days(7) > chrono::Utc::now()
        && instance_starts_at - chrono::Duration::days(7 * 6)
          > chrono::Utc::now()
    })
    .collect::<Vec<_>>();

  if instances.len() < original_count {
    tracing::debug!(
      "filtered {} event instance{} for event {} based on timing",
      original_count - instances.len(),
      if original_count - instances.len() == 1 {
        ""
      } else {
        "s"
      },
      event.id
    );
  }

  progress.inc(1);
  let permit = semaphore.acquire().await.unwrap();
  let resource_requests = client
    .fetch_event_resource_requests(&event.id)
    .await?
    .to_data_vec()?;
  drop(permit);

  let mut resourse_requests_with_resources = Vec::new();
  progress.inc_length(resource_requests.len() as _);
  for resource_request in resource_requests {
    let resource_request_with_resource =
      PcoResourceRequest::from_object(resource_request)
        .ok_or_eyre("failed to build `PcoResourceRequest` from `PcoObject`")?
        .fetch_resource(client.clone())
        .await
        .map_err(|e| {
          tracing::error!("failed to fetch_accompanying resource: {e}");
          e
        })?;
    resourse_requests_with_resources.push(resource_request_with_resource);
    progress.inc(1);
  }

  // iterate through the instances
  progress.inc_length(instances.len() as _);
  for instance in instances {
    tx.send(
      PcoInstancedEvent::from_pieces(
        event.clone(),
        instance.clone(),
        resourse_requests_with_resources.clone(),
      )
      .ok_or_eyre("failed to create instanced event from pieces")
      .map_err(|e| {
        tracing::error!(
          "failed to create instanced event from pieces: event = {:?}, \
           instance = {:?}, resource_requests = {:?}",
          event.clone(),
          instance.clone(),
          resourse_requests_with_resources.clone()
        );
        e
      })?,
    )
    .await?;
    progress.inc(1);
  }

  Ok::<(), Report>(())
}

pub async fn fetch_all_instanced_events() -> Result<Vec<PcoInstancedEvent>> {
  let secrets = crate::secrets::Secrets::new()?;
  let client = self::client::PcoClient::new(secrets);

  tracing::info!("fetching events...");
  let events = client.fetch_all_events().await?;
  // let events = events.split_off(events.len() - 100);
  tracing::info!("found {} events", events.len());

  let progress = Arc::new(indicatif::ProgressBar::hidden());
  progress.inc_length(events.len() as _);
  let semaphore = Arc::new(tokio::sync::Semaphore::new(10));
  let client = Arc::new(client);
  let (tx, mut rx) = mpsc::channel::<PcoInstancedEvent>(100);

  tokio::spawn({
    let progress = progress.clone();
    async move {
      tracing::info!("fetching event instances and resource requests...");
      for event in events {
        tokio::spawn({
          let (progress, semaphore, client, tx) = (
            progress.clone(),
            semaphore.clone(),
            client.clone(),
            tx.clone(),
          );
          async move {
            // fetch the instances
            let permit = semaphore.acquire().await.unwrap();
            let instances =
              client.fetch_instances(&event.id).await?.to_data_vec()?;
            drop(permit);
            progress.inc(1);

            if instances.is_empty() {
              return Ok::<(), Report>(());
            }

            handle_event_with_instances(
              progress, semaphore, client, tx, event, instances,
            )
            .await
            .map_err(|e| {
              tracing::error!("failed to handle event: {e:?}");
              e
            })
          }
        });
      }
    }
  });

  let mut instanced_events = Vec::new();
  while let Some(ie) = rx.recv().await {
    instanced_events.push(ie);
    if instanced_events.len() > 0 && instanced_events.len() % 50 == 0 {
      tracing::info!("found {} instanced events...", instanced_events.len());
    }
  }
  tracing::info!("found {} total instanced events", instanced_events.len());
  progress.finish();

  Ok(instanced_events)
}
