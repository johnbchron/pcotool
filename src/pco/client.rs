use std::{collections::HashMap, sync::Arc, time::Duration};

use color_eyre::{
  eyre::{OptionExt, Result, WrapErr},
  Section, SectionExt,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tracing::instrument;

use crate::secrets::Secrets;

const URL_PREFIX: &str = "https://api.planningcenteronline.com/calendar/v2";

#[derive(Clone)]
pub struct PcoClient {
  client:    reqwest::Client,
  secrets:   Secrets,
  semaphore: Arc<Semaphore>,
}

impl PcoClient {
  pub fn new(secrets: Secrets) -> Self {
    Self {
      client: reqwest::Client::new(),
      secrets,
      semaphore: Arc::new(Semaphore::new(5)),
    }
  }

  async fn fetch(&self, url: &str, mut retries: u8) -> Result<PcoResponse> {
    let _lock = self
      .semaphore
      .acquire()
      .await
      .wrap_err("failed to acquire semaphore")?;
    loop {
      if retries == 0 {
        tracing::debug!("fetching");
      } else {
        tracing::debug!("fetching - retries = {retries}");
      }

      // make the request
      let resp = self
        .client
        .get(url)
        .basic_auth(
          self.secrets.pco_app_id.clone(),
          Some(self.secrets.pco_secret.clone()),
        )
        .send()
        .await
        .wrap_err("failed to fetch from pco")?;

      // deserialize to PcoResponse
      let text_resp = resp
        .text()
        .await
        .wrap_err("failed to decode pco response as utf-8 plaintext")?;
      let json_resp = serde_json::from_str::<PcoResponse>(&text_resp)
        .wrap_err("failed to parse pco json response")
        .with_section(|| text_resp.header("Original Response:"))?;

      // handle 429 and retries
      if let PcoResponse::Error { ref errors } = json_resp {
        if errors.first().map(|e| e.code.as_str()) == Some("429") {
          if retries < 10 {
            tokio::time::sleep(Duration::from_secs(10)).await;
            retries += 1;
            continue;
          } else {
            tracing::error!("exceeded max retry count (10), aborting");
            return Err(color_eyre::eyre::eyre!(
              "exceeded max request retry count"
            ));
          }
        }
      }

      return Ok(json_resp);
    }
  }

  /// Fetch events from the Planning Center Online API.
  #[instrument(skip(self))]
  pub async fn fetch_events(
    &self,
    offset: Option<usize>,
  ) -> Result<PcoResponse> {
    let url = format!("{URL_PREFIX}/events?offset={}", offset.unwrap_or(0));
    self.fetch(&url, 0).await
  }

  #[instrument(skip(self))]
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
      events.extend(response.into_data_vec()?);
    }
    Ok(events)
  }

  /// Fetch event instances from the Planning Center Online API.
  #[instrument(skip(self))]
  pub async fn fetch_instances(&self, event_id: &str) -> Result<PcoResponse> {
    let url = format!("{URL_PREFIX}/events/{event_id}/event_instances");
    self.fetch(&url, 0).await
  }

  /// Fetch resource bookings from the Planning Center Online API.
  #[instrument(skip(self))]
  pub async fn fetch_resource_bookings(
    &self,
    instance_id: &str,
  ) -> Result<PcoResponse> {
    let url =
      format!("{URL_PREFIX}/event_instances/{instance_id}/resource_bookings");
    self.fetch(&url, 0).await
  }

  /// Fetch event resource requests from the Planning Center Online API.
  #[instrument(skip(self))]
  pub async fn fetch_event_resource_requests(
    &self,
    event_id: &str,
  ) -> Result<PcoResponse> {
    let url =
      format!("{URL_PREFIX}/events/{event_id}/event_resource_requests",);
    self.fetch(&url, 0).await
  }

  /// Fetch a resource from the Planning Center Online API.
  #[instrument(skip(self))]
  pub async fn fetch_resource(&self, resource_id: &str) -> Result<PcoResponse> {
    let url = format!("{URL_PREFIX}/resources/{resource_id}");
    self.fetch(&url, 0).await
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PcoError {
  code:   String,
  title:  String,
  detail: String,
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
  Error {
    errors: Vec<PcoError>,
  },
}

impl PcoResponse {
  pub fn into_data_vec(self) -> Result<Vec<PcoObject>> {
    match self {
      PcoResponse::Single { data } => Ok(vec![data]),
      PcoResponse::Multiple { data, .. } => Ok(data),
      PcoResponse::Error { errors } => {
        let mut error = color_eyre::eyre::eyre!("asana error");
        for err in errors {
          error = error.section(format!("{:#?}", err));
        }
        Err(error)
      }
    }
  }
  pub fn into_single(self) -> Result<PcoObject> {
    match self {
      PcoResponse::Single { data } => Ok(data),
      PcoResponse::Multiple { .. } => {
        Err(color_eyre::eyre::eyre!("unexpected multi-item response"))
      }
      PcoResponse::Error { errors } => {
        let mut error = color_eyre::eyre::eyre!("asana error");
        for err in errors {
          error = error.section(format!("{:#?}", err));
        }
        Err(error)
      }
    }
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PcoObject {
  #[serde(rename = "type")]
  pub type_:         String,
  pub id:            String,
  #[serde(default)]
  pub attributes:    Option<HashMap<String, serde_json::Value>>,
  #[serde(default)]
  pub relationships: Option<HashMap<String, serde_json::Value>>,
  #[serde(default)]
  pub links:         Option<HashMap<String, Option<String>>>,
}
