use std::{collections::HashMap, sync::Arc};

use color_eyre::eyre::{OptionExt, Result, WrapErr};
use serde::{Deserialize, Serialize};

use crate::secrets::Secrets;

#[derive(Clone)]
pub struct PcoClient {
  client:  reqwest::Client,
  secrets: Secrets,
}

impl PcoClient {
  pub fn new(secrets: Secrets) -> Self {
    Self {
      client: reqwest::Client::new(),
      secrets,
    }
  }

  /// Fetch events from the Planning Center Online API.
  pub async fn fetch_events(
    &self,
    offset: Option<usize>,
  ) -> Result<PcoResponse> {
    let url = format!(
      "https://api.planningcenteronline.com/calendar/v2/events?offset={}",
      offset.unwrap_or(0)
    );

    let resp = self
      .client
      .get(&url)
      .basic_auth(
        self.secrets.pco_app_id.clone(),
        Some(self.secrets.pco_secret.clone()),
      )
      .send()
      .await
      .wrap_err("failed to fetch")?;

    let resp = resp
      .json::<PcoResponse>()
      .await
      .wrap_err("failed to parse")?;

    Ok(resp)
  }

  pub async fn fetch_all_events(&self) -> Result<Vec<PcoObject>> {
    let initial_response = self.fetch_events(None).await?;
    let total_count = match initial_response {
      PcoResponse::Multiple { meta, .. } => meta
        .get("total_count")
        .ok_or_eyre("missing total count")?
        .as_u64()
        .ok_or_eyre("total count is not a number")?,
      _ => color_eyre::eyre::bail!("unexpected single-item response"),
    };

    let semaphore = Arc::new(tokio::sync::Semaphore::new(5));
    let mut tasks = Vec::new();

    let client = Arc::new(self.clone());
    for offset in (0..total_count).step_by(25) {
      tasks.push(tokio::spawn({
        let client = client.clone();
        let semaphore = semaphore.clone();
        async move {
          let _permit = semaphore.acquire().await.unwrap();
          client.fetch_events(Some(offset.try_into().unwrap())).await
        }
      }))
    }

    let mut events = Vec::new();
    for task in tasks {
      let response = task
        .await
        .wrap_err("failed to join task")?
        .wrap_err("task failed")?;
      events.extend(response.to_data_vec());
    }
    Ok(events)
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum PcoResponse {
  Single {
    data: PcoObject,
  },
  Multiple {
    data:     Vec<PcoObject>,
    #[serde(default)]
    included: Vec<PcoObject>,
    #[serde(default)]
    links:    HashMap<String, String>,
    #[serde(default)]
    meta:     HashMap<String, serde_json::Value>,
  },
}

impl PcoResponse {
  pub fn to_data_vec(self) -> Vec<PcoObject> {
    match self {
      PcoResponse::Single { data } => vec![data],
      PcoResponse::Multiple { data, .. } => data,
    }
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PcoObject {
  #[serde(rename = "type")]
  pub type_:         String,
  pub id:            String,
  pub attributes:    Option<HashMap<String, serde_json::Value>>,
  pub relationships: Option<HashMap<String, serde_json::Value>>,
  pub links:         Option<HashMap<String, String>>,
}
