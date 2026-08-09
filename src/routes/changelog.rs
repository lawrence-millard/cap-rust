use axum::extract::Query;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct ChangelogParams {
    pub version: Option<String>,
}

pub async fn posts(_query: Query<ChangelogParams>) -> Json<Value> {
    Json(json!([]))
}

pub async fn status(_query: Query<ChangelogParams>) -> Json<Value> {
    Json(json!({ "hasUpdate": false }))
}
