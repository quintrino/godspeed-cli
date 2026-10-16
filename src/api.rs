use crate::task::TaskRequest;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::env;

type ApiResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize, Debug)]
struct ListsResponse {
    lists: Vec<NamedItem>,
}

#[derive(Deserialize, Debug)]
struct LabelsResponse {
    labels: Vec<NamedItem>,
}

#[derive(Deserialize, Debug)]
struct NamedItem {
    id: String,
    name: String,
}

/// The API endpoint for `path`. `GODSPEED_API_URL` replaces the base URL so the tests can point
/// the binary at a local mock server; the API token is sent to whatever it names.
fn api_url(path: &str) -> String {
    let base = env::var("GODSPEED_API_URL").unwrap_or_else(|_| "https://api.godspeedapp.com".to_string());
    format!("{}/{path}", base.trim_end_matches('/'))
}

fn name_to_id(items: Vec<NamedItem>) -> HashMap<String, String> {
    items.into_iter().map(|item| (item.name.to_lowercase(), item.id)).collect()
}

/// All lists, as lowercase name -> id.
pub fn fetch_lists(api_key: &str) -> ApiResult<HashMap<String, String>> {
    let response = reqwest::blocking::Client::new()
        .get(api_url("lists"))
        .header("Authorization", format!("Bearer {}", api_key))
        .send()?;

    let lists: ListsResponse = response.json()?;
    Ok(name_to_id(lists.lists))
}

/// All labels, as lowercase name -> id.
pub fn fetch_labels(api_key: &str) -> ApiResult<HashMap<String, String>> {
    let response = reqwest::blocking::Client::new()
        .get(api_url("labels"))
        .header("Authorization", format!("Bearer {}", api_key))
        .send()?;

    let labels: LabelsResponse = response.json()?;
    Ok(name_to_id(labels.labels))
}

/// One page of `GET /tasks`. The API also returns the lists and labels the tasks use.
#[derive(Deserialize, Debug, Default)]
pub struct TaskPage {
    #[serde(default)]
    pub tasks: Vec<Value>,
    #[serde(default)]
    pub lists: Vec<Value>,
    #[serde(default)]
    pub labels: Vec<Value>,
}

pub enum PageError {
    /// HTTP 429, with the `Retry-After` delay in seconds if the API gave one.
    RateLimited(Option<u64>),
    Failed(Box<dyn std::error::Error>),
}

/// Fetches up to 250 tasks with the given status (`incomplete` or `complete`), newest update
/// first. `updated_before` continues from an earlier page, and `updated_after` limits the tasks
/// to those changed since then.
pub fn fetch_task_page(
    api_key: &str,
    status: &str,
    updated_before: Option<&str>,
    updated_after: Option<&str>,
) -> Result<TaskPage, PageError> {
    let mut query = vec![("status", status)];
    if let Some(before) = updated_before {
        query.push(("updated_before", before));
    }
    if let Some(after) = updated_after {
        query.push(("updated_after", after));
    }
    let response = reqwest::blocking::Client::new()
        .get(api_url("tasks"))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Accept", "application/json")
        .query(&query)
        .send()
        .map_err(|e| PageError::Failed(Box::new(e)))?;

    let status = response.status();
    if status.as_u16() == 429 {
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse().ok());
        return Err(PageError::RateLimited(retry_after));
    }
    if !status.is_success() {
        return Err(PageError::Failed(format!("API error: {status}").into()));
    }
    response.json().map_err(|e| PageError::Failed(Box::new(e)))
}

/// Creates the task and returns what the API answered (`{}` if the answer wasn't JSON).
pub fn send_task(task: &TaskRequest, api_key: &str) -> ApiResult<Value> {
    let response = reqwest::blocking::Client::new()
        .post(api_url("tasks"))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(task)
        .send()?;

    if response.status().is_success() {
        Ok(response.json().unwrap_or_else(|_| serde_json::json!({})))
    } else {
        Err(format!("API error: {}", response.status()).into())
    }
}
