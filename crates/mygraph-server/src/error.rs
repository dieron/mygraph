//! Error responses — spec/api/errors.md.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mygraph_core::engine::OpError;
use serde_json::{Value as Json, json};

/// A failed request: reason code, message and the members of [ERR-1]. Boxed, so results that
/// carry it stay small.
#[derive(Debug, Clone)]
pub struct ApiError(Box<ErrorBody>);

#[derive(Debug, Clone)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    pub section: Option<String>,
    pub index: Option<usize>,
    pub details: Json,
    /// `Retry-After` seconds, for `graph.warming_up`.
    pub retry_after: Option<u64>,
}

impl ApiError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self(Box::new(ErrorBody {
            code: code.to_owned(),
            message: message.into(),
            section: None,
            index: None,
            details: Json::Null,
            retry_after: None,
        }))
    }

    pub fn status(&self) -> StatusCode {
        status_of(&self.code)
    }
}

impl From<OpError> for ApiError {
    fn from(e: OpError) -> Self {
        Self(Box::new(ErrorBody {
            code: e.reason.code().to_owned(),
            message: e.message,
            section: e.section,
            index: e.index,
            details: e.details,
            retry_after: None,
        }))
    }
}

/// [ERR-3]
pub fn status_of(code: &str) -> StatusCode {
    match code {
        "auth.missing_key" | "auth.invalid_key" => StatusCode::UNAUTHORIZED,
        "auth.forbidden" | "auth.graph_forbidden" => StatusCode::FORBIDDEN,
        "ref.not_found" | "graph.not_found" | "api.not_found" | "content.not_found"
        | "blob.not_found" => StatusCode::NOT_FOUND,
        "api.method_not_allowed" => StatusCode::METHOD_NOT_ALLOWED,
        "write.version_conflict"
        | "write.property_conflict"
        | "write.ambiguous_match"
        | "write.type_mismatch"
        | "write.link_mismatch"
        | "key.taken"
        | "lifecycle.identity_taken"
        | "lifecycle.retracted"
        | "lifecycle.invalid_transition"
        | "link.endpoint_not_active"
        | "citation.evidence_not_active"
        | "citation.target_not_active"
        | "evidence.blob_conflict"
        | "graph.exists" => StatusCode::CONFLICT,
        "api.body_too_large" => StatusCode::PAYLOAD_TOO_LARGE,
        "graph.warming_up" | "search.semantic_unavailable" => StatusCode::SERVICE_UNAVAILABLE,
        "graph.load_failed" | "internal" => StatusCode::INTERNAL_SERVER_ERROR,
        c if c.starts_with("storage.") => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::BAD_REQUEST,
    }
}

/// [ERR-4]
pub fn hint_of(code: &str) -> Option<&'static str> {
    Some(match code {
        "ref.not_found" => "Find the object's ID with search or resolve.",
        "write.ambiguous_match" => {
            "Several objects match; pass an ID or a key from details.candidates, or set create_new: true."
        }
        "write.evidence_required" => {
            "Cite evidence for each new fact; for your own conclusion, add an observation evidence and cite it."
        }
        "write.version_conflict" => {
            "The object changed since you read it; get it again and retry with its current version."
        }
        "write.property_conflict" => {
            "A stored value differs; use on_conflict: overwrite to replace it, or keep to leave it and get a warning."
        }
        "lifecycle.retracted" => {
            "The object is retracted; restore it with retract and action: restore first."
        }
        "vocab.not_styled" => {
            "This graph wants names in its style; resend with details.expected (overview lists the names in use)."
        }
        "graph.not_found" => "List the available graphs with GET /v1/graphs.",
        "graph.warming_up" => {
            "The graph is loading; retry after the number of seconds in Retry-After."
        }
        "search.semantic_unavailable" => {
            "Search with mode auto (words, and meaning when available) or text instead."
        }
        "auth.missing_key" => "Send Authorization: Bearer <key>.",
        "api.not_found" => "See GET /openapi.yaml for the available routes.",
        "json.malformed" => "The body must be one JSON object.",
        c if c.ends_with(".invalid_field") => {
            "The message names the field; see the operation's specification."
        }
        _ => return None,
    })
}

impl std::ops::Deref for ApiError {
    type Target = ErrorBody;
    fn deref(&self) -> &ErrorBody {
        &self.0
    }
}

impl std::ops::DerefMut for ApiError {
    fn deref_mut(&mut self) -> &mut ErrorBody {
        &mut self.0
    }
}

/// The error code of an error answer, as a response extension ([ADMIN-MET-1] counts them).
#[derive(Clone, Debug)]
pub struct ErrorCode(pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let e = *self.0;
        let body = json!({
            "error": {
                "code": e.code, "message": e.message, "section": e.section,
                "index": e.index, "details": e.details, "hint": hint_of(&e.code),
            }
        });
        let mut r = (status, crate::api::json_body(&body)).into_response();
        r.extensions_mut().insert(ErrorCode(e.code));
        let h = r.headers_mut();
        if status == StatusCode::UNAUTHORIZED {
            h.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        if let Some(s) = e.retry_after {
            h.insert(header::RETRY_AFTER, HeaderValue::from(s));
        }
        r
    }
}
