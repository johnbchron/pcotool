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
