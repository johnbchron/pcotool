use std::fmt::Debug;

use color_eyre::{
  eyre::{Result, WrapErr},
  Section,
};
use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::secrets::Secrets;

/// The error type returned by the Asana API.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AsanaError {
  pub message: String,
  pub help:    Option<String>,
  pub phrase:  Option<String>,
}

/// A generic wrapper type around Asana responses.
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
  /// Convert the response into a [`color_eyre`] result.
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

/// A client for interacting with the Asana API.
pub struct AsanaClient {
  client:  reqwest::Client,
  secrets: Secrets,
}

impl AsanaClient {
  /// Create a new Asana client.
  pub fn new(secrets: Secrets) -> Self {
    Self {
      client: reqwest::Client::new(),
      secrets,
    }
  }

  /// Fetch a task from the Asana API.
  #[instrument(skip(self))]
  pub async fn get_task(&self, task_gid: u64) -> Result<AsanaTask> {
    let url = format!("https://app.asana.com/api/1.0/tasks/{task_gid}");

    let resp = self
      .client
      .get(&url)
      .bearer_auth(&self.secrets.asana_pat)
      .query(&[("opt_fields", "name,html_notes")])
      .send()
      .await
      .wrap_err("failed to fetch")?;

    let resp = resp
      .json::<AsanaResponse<AsanaTask>>()
      .await
      .wrap_err("failed to parse")?
      .into_result()?;

    Ok(resp)
  }
}

/// A task from the Asana API.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AsanaTask {
  pub gid:        String,
  pub name:       String,
  pub html_notes: String,
}

impl AsanaTask {
  /// Extract the PCO event id from the html notes of the task.
  pub fn into_linked_task(self) -> Option<AsanaLinkedTask> {
    use scraper::{Html, Selector};

    let fragment = Html::parse_fragment(&self.html_notes);
    let selector = Selector::parse("code").unwrap();

    let code_element = fragment
      .select(&selector)
      .next()?
      .text()
      .collect::<String>();

    // extract the event id with the following format:
    // `>>>>> [event_id] <<<<<`
    let start = code_element.find(">>>>>")? + 6;
    let end = code_element.find("<<<<<")?;
    let event_id = code_element[start..end].trim().parse().ok()?;

    Some(AsanaLinkedTask {
      task: self,
      event_id,
    })
  }
}

/// A task from the Asana API that has been linked to a PCO event.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AsanaLinkedTask {
  pub task:     AsanaTask,
  pub event_id: u64,
}
