use crate::task::TaskRequest;
use serde::Deserialize;
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

pub fn send_task(task: &TaskRequest, api_key: &str) -> ApiResult<()> {
    let response = reqwest::blocking::Client::new()
        .post(api_url("tasks"))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(task)
        .send()?;

    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("API error: {}", response.status()).into())
    }
}
