mod secrets;

use std::{collections::HashMap, sync::Arc};

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

  let secrets = Arc::new(secrets::Secrets::new()?);
  tracing::info!("loaded secrets");

  tracing::info!("making a request to httpbin");
  let resp = reqwest::get("https://httpbin.org/ip")
    .await?
    .json::<HashMap<String, String>>()
    .await?;
  println!("{resp:#?}");
  Ok(())
}
