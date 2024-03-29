use std::{collections::HashMap, sync::Arc};

use color_eyre::eyre::Result;
use tracing::instrument;

mod secrets {
  use std::env::var;

  use color_eyre::eyre::{Result, WrapErr};

  #[derive(Debug, Clone)]
  pub struct Secrets {
    pub asana_pat:  String,
    pub pco_app_id: String,
    pub pco_secret: String,
  }

  impl Secrets {
    pub fn new() -> Result<Self> {
      Ok(Self {
        asana_pat:  var("ASANA_PAT").wrap_err("ASANA_PAT is not set")?,
        pco_app_id: var("PCO_APP_ID").wrap_err("PCO_APP_ID is not set")?,
        pco_secret: var("PCO_SECRET").wrap_err("PCO_SECRET is not set")?,
      })
    }
  }
}

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
