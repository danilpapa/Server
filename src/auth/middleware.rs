use crate::auth::jwt::verify_access_jwt;
use axum::{
    extract::FromRequestParts,
    http::{StatusCode, request::Parts},
};
use uuid::Uuid;

pub struct AuthUser {
    pub user_id: Uuid,
}

impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let auth_header = parts
            .headers
            .get("Authorization")
            .and_then(|value| value.to_str().ok());
        if let Some(header) = auth_header {
            if header.starts_with("Bearer ") {
                let token = header.trim_start_matches("Bearer ");
                if let Ok(payload) = verify_access_jwt(token) {
                    if let Ok(user_id) = Uuid::parse_str(&payload.sub) {
                        return Ok(AuthUser { user_id });
                    }
                }
            }
        }
        Err((StatusCode::UNAUTHORIZED, "Missing or invalid Bearer token"))
    }
}
