mod asana;
mod secrets;

use color_eyre::eyre::Result;
use tracing::instrument;

fn install_tracing() {
  use tracing_error::ErrorLayer;
  use tracing_subscriber::{fmt, prelude::*, EnvFilter};

  let fmt_layer = fmt::layer().with_target(false);
  let filter_layer = EnvFilter::try_from_default_env()
    .or_else(|_| EnvFilter::try_new("info"))
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
  tracing::info!("loaded secrets");

  let client = asana::AsanaClient::new(secrets.clone());
  tracing::info!("created asana client");

  let task = client.get_task(1206957347414555).await?;
  tracing::info!("task: {task:#?}");

  Ok(())
}
