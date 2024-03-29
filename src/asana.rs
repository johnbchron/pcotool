use std::fmt::Debug;

use color_eyre::{
  eyre::{Result, WrapErr},
  Section,
};
use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::secrets::Secrets;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AsanaError {
  pub message: String,
  pub help:    Option<String>,
  pub phrase:  Option<String>,
}

/// A generic wrapper type around asana responses.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum AsanaResponse<T>
where
  T: Debug,
{
  Success { data: T },
  Error { errors: Vec<AsanaError> },
}

impl<T> AsanaResponse<T>
where
  T: Debug,
{
  pub fn into_result(self) -> Result<T> {
    match self {
      AsanaResponse::Success { data } => Ok(data),
      AsanaResponse::Error { errors } => {
        let mut error = color_eyre::eyre::eyre!("asana error");
        for err in errors {
          error = error.section(format!("{:#?}", err));
        }
        Err(error)
      }
    }
  }
}

pub struct AsanaClient {
  client:  reqwest::Client,
  secrets: Secrets,
}

impl AsanaClient {
  pub fn new(secrets: Secrets) -> Self {
    Self {
      client: reqwest::Client::new(),
      secrets,
    }
  }

  #[instrument(skip(self))]
  pub async fn get_task(&self, task_gid: u64) -> Result<AsanaTask> {
    let url = format!("https://app.asana.com/api/1.0/tasks/{task_gid}");
    tracing::info!("fetching task from {url}");

    let resp = self
      .client
      .get(&url)
      .bearer_auth(&self.secrets.asana_pat)
      .query(&[("opt_fields", "name,html_notes")])
      .send()
      .await
      .wrap_err("failed to fetch")?;

    // tracing::info!("response: {}", resp.text().await?);
    // todo!()
    let resp = resp
      .json::<AsanaResponse<AsanaTask>>()
      .await
      .wrap_err("failed to parse")?
      .into_result()?;

    Ok(resp)
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AsanaTask {
  pub gid:        String,
  pub name:       String,
  pub html_notes: String,
}
