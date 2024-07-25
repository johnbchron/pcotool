mod asana;
mod pco;
mod secrets;

use std::sync::Arc;

use color_eyre::eyre::{Context, Error, Result};
use secrets::Secrets;
use tracing::instrument;

use crate::pco::PcoInstancedEvent;

pub type DateTimeUtc = chrono::DateTime<chrono::Utc>;

fn setup_tracing() {
  use tracing_error::ErrorLayer;
  use tracing_subscriber::{fmt, prelude::*, EnvFilter};

  let fmt_layer = fmt::layer().with_target(false);
  let filter_layer = EnvFilter::try_from_default_env()
    .or_else(|_| EnvFilter::try_new("pcotool=info"))
    .unwrap();

  tracing_subscriber::registry()
    .with(filter_layer)
    .with(fmt_layer)
    .with(ErrorLayer::default())
    .init();
}

#[tokio::main]
#[instrument]
async fn main() -> Result<()> {
  setup_tracing();
  color_eyre::install()?;

  let instanced_events = self::pco::fetch_all_instanced_events().await?;
  std::fs::write(
    "instanced_events.json",
    serde_json::to_string(&instanced_events).unwrap(),
  )
  .unwrap();

  // let json_data: Vec<u8> = std::fs::read("instanced_events.json").unwrap();
  // let instanced_events: Vec<PcoInstancedEvent> =
  //   serde_json::from_slice(&json_data).unwrap();

  let asana_client = crate::asana::AsanaClient::new(Secrets::new()?);
  let tasks = asana_client
    .get_all_tasks()
    .await?
    .into_iter()
    .filter_map(|t| t.into_linked_task())
    .collect::<Vec<_>>();
  tracing::info!("tasks: {tasks:?}");

  tracing::info!("sorting tasks...");
  let mut tasks_to_update: Vec<(PcoInstancedEvent, asana::AsanaLinkedTask)> =
    Vec::new();
  let mut tasks_to_create: Vec<PcoInstancedEvent> = Vec::new();

  for instanced_event in instanced_events {
    if let Some(task) = tasks.iter().find(|t| {
      t.event_id == instanced_event.event_id
        && t.instance_id == instanced_event.instance_id
    }) {
      tasks_to_update.push((instanced_event, task.clone()));
    } else {
      tasks_to_create.push(instanced_event);
    }
  }

  tracing::info!(
    "sorted tasks; {} task{} to create, {} task{} to update",
    tasks_to_create.len(),
    s(tasks_to_create.len()),
    tasks_to_update.len(),
    s(tasks_to_update.len()),
  );

  tracing::info!("creating tasks...");
  let asana_client = Arc::new(asana_client);

  let mut jhs = Vec::new();
  for ie in tasks_to_create {
    jhs.push(tokio::spawn({
      let asana_client = asana_client.clone();
      async move {
        asana_client
          .create_task(ie)
          .await
          .wrap_err("failed to create task")?;
        Ok::<(), Error>(())
      }
    }));
  }

  futures::future::join_all(jhs).await;

  Ok(())
}

fn s(count: impl Into<usize>) -> &'static str {
  if count.into() != 1 {
    "s"
  } else {
    ""
  }
}
