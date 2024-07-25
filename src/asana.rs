use std::fmt::Debug;

use color_eyre::{
  eyre::{Result, WrapErr},
  Section, SectionExt,
};
use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::{pco::PcoInstancedEvent, secrets::Secrets};

/// The error type returned by the Asana API.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AsanaError {
  pub message: String,
  pub help:    Option<String>,
  pub phrase:  Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AsanaNextPage {
  pub offset: String,
  pub path:   String,
  pub uri:    String,
}

/// A generic wrapper type around Asana responses.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum AsanaResponse<T>
where
  T: Debug,
{
  Success {
    data:      T,
    next_page: Option<AsanaNextPage>,
  },
  Error {
    errors: Vec<AsanaError>,
  },
}

fn error_from_asana_errors(
  errors: Vec<AsanaError>,
) -> color_eyre::eyre::Report {
  let mut error = color_eyre::eyre::eyre!("asana error");
  for err in errors {
    error = error.section(format!("{:#?}", err));
  }
  error
}

impl<T> AsanaResponse<T>
where
  T: Debug,
{
  /// Convert the response into a [`color_eyre`] result.
  pub fn into_result(self) -> Result<T> {
    match self {
      AsanaResponse::Success { data, .. } => Ok(data),
      AsanaResponse::Error { errors } => Err(error_from_asana_errors(errors)),
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

  /// Fetch tasks in the PCOTool project from the Asana API.
  #[instrument(skip(self))]
  async fn get_tasks(
    &self,
    offset: Option<&str>,
  ) -> Result<AsanaResponse<Vec<AsanaTask>>> {
    let url = format!(
      "https://app.asana.com/api/1.0/projects/{project_gid}/tasks",
      project_gid = self.secrets.asana_project_gid
    );

    let mut req = self
      .client
      .get(&url)
      .bearer_auth(&self.secrets.asana_pat)
      .query(&[("opt_fields", "name,html_notes"), ("limit", "25")]);
    if let Some(offset) = offset {
      req = req.query(&[("offset", offset)])
    }

    let resp = req
      .send()
      .await
      .wrap_err("failed to fetch")?
      .json::<AsanaResponse<Vec<AsanaTask>>>()
      .await
      .wrap_err("failed to parse")?;

    Ok(resp)
  }

  /// Fetch all tasks in the PCOTool project from the Asana API.
  #[instrument(skip(self))]
  pub async fn get_all_tasks(&self) -> Result<Vec<AsanaTask>> {
    let mut resp = self.get_tasks(None).await?;
    let mut results: Vec<AsanaTask> = vec![];

    loop {
      let (mut data, next_page) = match resp {
        AsanaResponse::Success { data, next_page } => (data, next_page),
        AsanaResponse::Error { errors } => {
          return Err(error_from_asana_errors(errors));
        }
      };

      results.append(&mut data);
      let Some(next_page) = next_page else {
        break;
      };

      resp = self.get_tasks(Some(&next_page.offset)).await?;
    }

    Ok(results)
  }

  /// Create an Asana task from a [`PcoInstancedEvent`].
  #[instrument(skip(self))]
  pub async fn create_task(
    &self,
    ie: PcoInstancedEvent,
  ) -> Result<AsanaResponse<AsanaTask>> {
    let url = "https://app.asana.com/api/1.0/tasks";

    let local_starts_at =
      ie.starts_at.with_timezone(&chrono_tz::America::Chicago);
    let name = format!(
      "{} - {}",
      ie.event_name.trim(),
      local_starts_at.format("%m-%d-%Y")
    );
    let html_notes = format!(
      "<body>\n<code>&gt;&gt;&gt;&gt;&gt; {event_id}:{instance_id} \
       &lt;&lt;&lt;&lt;&lt;</code></body>",
      event_id = ie.event_id,
      instance_id = ie.instance_id
    );

    let request_payload = serde_json::json!({
      "data": {
        "projects": [
          self.secrets.asana_project_gid.to_string()
        ],
        "name": name,
        "due_at": ie.starts_at.to_rfc3339(),
        "html_notes": html_notes,
      },
    });

    let req = self
      .client
      .post(url)
      .query(&[("opt_fields", "name,html_notes")])
      .bearer_auth(&self.secrets.asana_pat)
      .json(&request_payload);

    let resp = req.send().await.wrap_err("failed to fetch")?;

    // deserialize to PcoResponse
    let text_resp = resp
      .text()
      .await
      .wrap_err("failed to decode asana response as utf-8 plaintext")?;
    let json_resp =
      serde_json::from_str::<AsanaResponse<AsanaTask>>(&text_resp)
        .wrap_err("failed to parse asana json response")
        .with_section(|| text_resp.header("Original Response:"))?;

    Ok(json_resp)
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
  /// Convert the task into a linked task.
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
    let ids: Vec<&str> = code_element[start..end].trim().split(':').collect();

    // make sure we've got both IDs
    if ids.len() != 2 {
      return None;
    }
    if ids[0].is_empty() || ids[1].is_empty() {
      return None;
    }

    Some(AsanaLinkedTask {
      task:        self,
      event_id:    ids[0].to_string(),
      instance_id: ids[1].to_string(),
    })
  }

  /// Convert the task into a linked task.
  pub fn as_linked_task(&self) -> Option<AsanaLinkedTask> {
    self.clone().into_linked_task()
  }
}

/// A task from the Asana API that has been linked to a PCO event.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AsanaLinkedTask {
  pub task:        AsanaTask,
  pub event_id:    String,
  pub instance_id: String,
}
