use std::env::var;

use color_eyre::eyre::{Result, WrapErr};

#[derive(Debug, Clone)]
pub struct Secrets {
  pub asana_pat:           String,
  pub asana_workspace_gid: u64,
  pub asana_project_gid:   u64,
  pub pco_app_id:          String,
  pub pco_secret:          String,
}

impl Secrets {
  #[tracing::instrument(level = "debug")]
  pub fn new() -> Result<Self> {
    Ok(Self {
      asana_pat:           var("ASANA_PAT").wrap_err("ASANA_PAT is not set")?,
      asana_workspace_gid: var("ASANA_WORKSPACE_GID")
        .wrap_err("ASANA_WORKSPACE_GID is not set")?
        .parse()
        .wrap_err("ASANA_WORKSPACE_GID cannot be parsed as a number")?,
      asana_project_gid:   var("ASANA_PROJECT_GID")
        .wrap_err("ASANA_PROJECT_GID is not set")?
        .parse()
        .wrap_err("ASANA_PROJECT_GID cannot be parsed as a number")?,
      pco_app_id:          var("PCO_APP_ID")
        .wrap_err("PCO_APP_ID is not set")?,
      pco_secret:          var("PCO_SECRET")
        .wrap_err("PCO_SECRET is not set")?,
    })
  }
}
