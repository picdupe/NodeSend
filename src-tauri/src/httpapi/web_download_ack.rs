use axum::{extract::Path, http::StatusCode};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

static ACKS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

fn acknowledgements() -> &'static Mutex<HashMap<String, Instant>> {
    ACKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn valid_token(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub async fn acknowledge(Path(token): Path<String>) -> StatusCode {
    if !valid_token(&token) {
        return StatusCode::BAD_REQUEST;
    }
    let mut acks = acknowledgements()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let now = Instant::now();
    acks.retain(|_, created| now.duration_since(*created) < Duration::from_secs(30));
    acks.insert(token, now);
    StatusCode::NO_CONTENT
}

pub async fn poll(Path(token): Path<String>) -> StatusCode {
    if !valid_token(&token) {
        return StatusCode::BAD_REQUEST;
    }
    let mut acks = acknowledgements()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let now = Instant::now();
    acks.retain(|_, created| now.duration_since(*created) < Duration::from_secs(30));
    if acks.remove(&token).is_some() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}
