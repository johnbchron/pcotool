#![feature(result_flattening)]

mod asana;
mod pco;
mod secrets;

use std::{collections::HashSet, ops::Deref, sync::Arc};

use color_eyre::eyre::{Context, Error, Result};
use futures::TryStreamExt;
use pco::PcoEventWithRequestedResources;
use secrets::Secrets;
use serde::{Deserialize, Serialize};
use tracing::instrument;

pub type DateTimeUtc = chrono::DateTime<chrono::Utc>;

fn setup_tracing() {
  use tracing_error::ErrorLayer;
  use tracing_subscriber::{fmt, prelude::*, EnvFilter};

  let fmt_layer = fmt::layer().with_target(false);
  let filter_layer = EnvFilter::try_from_default_env()
    .or_else(|_| EnvFilter::try_new("pcotool=debug"))
    .unwrap();

  tracing_subscriber::registry()
    .with(filter_layer)
    .with(fmt_layer)
    .with(ErrorLayer::default())
    .init();
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CanonTask {
  event_id:      String,
  name:          String,
  resource_tags: HashSet<asana::AsanaTag>,
}

async fn pco_items_to_canon_task(
  event: PcoEventWithRequestedResources,
  asana_client: impl Deref<Target = asana::AsanaClient>,
) -> Result<CanonTask> {
  let name = event.event.get_attribute("name").unwrap_or_else(|| {
    tracing::warn!("failed to find event name for event: {}", event.event.id);
    format!("Nameless Asana Event: {}", event.event.id)
  });

  let resource_names = event
    .requested_resources
    .iter()
    .filter_map(|r| {
      let name = r.get_attribute("name");
      if name.is_none() {
        tracing::warn!("failed to find resource name for resource: {}", r.id);
      }
      name
    })
    .filter_map(|name| crate::asana::TRACKED_RESOURCES.get(name.as_str()))
    .map(|v| v.to_string())
    .collect::<Vec<String>>();

  let mut resource_tags = Vec::new();
  for resource in resource_names {
    let tag = asana_client.find_tag_by_name(&resource).await?;
    match tag {
      Some(tag) => resource_tags.push(tag),
      None => tracing::warn!("failed to find tag for resource: {}", resource),
    }
  }
  let resource_tags = resource_tags.into_iter().collect();

  Ok(CanonTask {
    event_id: event.event.id.clone(),
    name,
    resource_tags,
  })
}

#[tokio::main]
#[instrument]
async fn main() -> Result<()> {
  setup_tracing();
  color_eyre::install()?;

  let asana_client = Arc::new(crate::asana::AsanaClient::new(Secrets::new()?));

  let pco_client = Arc::new(pco::client::PcoClient::new(Secrets::new()?));
  let events = pco::find_relevant_events(pco_client.clone())
    .await
    .and_then({
      let asana_client = asana_client.clone();
      move |item| {
        let asana_client = asana_client.clone();
        pco_items_to_canon_task(item, asana_client)
      }
    })
    .try_collect::<Vec<_>>()
    .await?;

  // std::fs::write("events.json", serde_json::to_string(&events).unwrap())
  //   .unwrap();

  // let json_data: Vec<u8> = std::fs::read("events.json").unwrap();
  // let events: Vec<CanonTask> = serde_json::from_slice(&json_data).unwrap();

  tracing::info!("fetching tasks...");
  let tasks = asana_client.get_all_tasks().await?;
  tracing::info!("fetched {} task{}", tasks.len(), s(tasks.len()));
  let tasks = tasks
    .into_iter()
    .filter_map(|t| t.into_linked_task())
    .collect::<Vec<_>>();
  tracing::info!("fetched {} task{}", tasks.len(), s(tasks.len()));

  tracing::info!("sorting tasks...");
  let mut tasks_to_update: Vec<(CanonTask, asana::AsanaLinkedTask)> =
    Vec::new();
  let mut tasks_to_create: Vec<CanonTask> = Vec::new();

  for canon_task in events {
    if let Some(task) = tasks
      .iter()
      .find(|t| t.canon_task.event_id == canon_task.event_id)
    {
      tasks_to_update.push((canon_task, task.clone()));
    } else {
      tasks_to_create.push(canon_task);
    }
  }

  let tasks_to_create = tasks_to_create
    .into_iter()
    .filter(|t| !t.resource_tags.is_empty())
    .collect::<Vec<_>>();
  let tasks_to_update = tasks_to_update
    .into_iter()
    .filter(|(t, _)| !t.resource_tags.is_empty())
    .filter(|(canon_task, asana_task)| &asana_task.canon_task != canon_task)
    .collect::<Vec<_>>();

  tracing::info!(
    "sorted tasks; {} task{} to create, {} task{} to update",
    tasks_to_create.len(),
    s(tasks_to_create.len()),
    tasks_to_update.len(),
    s(tasks_to_update.len()),
  );

  if !tasks_to_create.is_empty() {
    tracing::info!("creating tasks...");
  }

  let mut join_handles = Vec::new();
  for canon_task in tasks_to_create {
    join_handles.push(tokio::spawn({
      let asana_client = asana_client.clone();
      async move {
        async move {
          asana_client
            .create_task(canon_task)
            .await
            .wrap_err("failed to create task")?;
          Ok::<_, Error>(())
        }
        .await
        .map_err(|e| {
          tracing::error!("{:?}", e);
        })
      }
    }));
  }
  futures::future::join_all(join_handles).await;

  if !tasks_to_update.is_empty() {
    tracing::info!("updating tasks...");
  }

  let mut join_handles = Vec::new();
  for (canon_task, task) in tasks_to_update {
    join_handles.push(tokio::spawn({
      let asana_client = asana_client.clone();
      async move {
        async move {
          asana_client
            .update_task(task, canon_task)
            .await
            .wrap_err("failed to update task")?;
          Ok::<_, Error>(())
        }
        .await
        .map_err(|e| {
          tracing::error!("{:?}", e);
        })
      }
    }));
  }
  futures::future::join_all(join_handles).await;

  Ok(())
}

fn s(count: impl Into<usize>) -> &'static str {
  if count.into() != 1 {
    "s"
  } else {
    ""
  }
}
