mod asana;
mod pco;
mod secrets;

use color_eyre::eyre::{OptionExt, Result};
use tracing::instrument;

fn install_tracing() {
  use tracing_error::ErrorLayer;
  use tracing_subscriber::{fmt, prelude::*, EnvFilter};

  let fmt_layer = fmt::layer().with_target(false);
  let filter_layer = EnvFilter::try_from_default_env()
    .or_else(|_| EnvFilter::try_new("pcotool=debug,info"))
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
  install_tracing();
  color_eyre::install()?;

  let secrets = secrets::Secrets::new()?;

  let asana_client = asana::AsanaClient::new(secrets.clone());
  let pco_client = pco::PcoClient::new(secrets.clone());

  // let task = asana_client.get_task(1206957347414555).await?;
  // let linked_task = task.as_linked_task().ok_or_eyre("task is not linked")?;
  // tracing::info!("fetched task: {:?}", linked_task);

  let events = pco_client.fetch_all_events().await?;
  tracing::info!("fetched {} events", events.len());

  Ok(())
}
