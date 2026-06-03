use crate::auth::middleware::AuthUser;
use crate::controllers::models::update_user_request_body::UpdateUserRequestBody;
use crate::controllers::models::user_name_search_query::UserNameSearchQuery;
use crate::controllers::models::user_response::UserResponse;
use crate::entities::{User, UserActiveModel, UserColumn};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    routing::get,
};
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use uuid::Uuid;

pub fn router() -> Router<DatabaseConnection> {
    Router::new()
        .route("/users/me", get(get_me).patch(update_me))
        .route("/users/{id}", get(get_user_by_id))
        .route("/users/search", get(search_users))
}

#[utoipa::path(
    get,
    path = "/users/me",
    responses(
        (status = 200, description = "Current user profile retrieved successfully", body = UserResponse),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 404, description = "User profile not found")
    ),
    security(
        ("bearer_auth" = [])
    ),
    tag = "Users"
)]
pub async fn get_me(
    auth_user: AuthUser,
    State(db_connection): State<DatabaseConnection>,
) -> Result<Json<UserResponse>, (StatusCode, String)> {
    let user_id = auth_user.user_id;

    let model = User::find_by_id(user_id)
        .one(&db_connection)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Unable to retrieve your profile. Please try again.".to_string()))?;

    let Some(model) = model else {
        return Err((StatusCode::NOT_FOUND, "Your profile could not be found.".to_string()));
    };

    Ok(Json(UserResponse {
        id: model.id,
        username: model.username,
        avatar_url: model.avatar_url,
        bio: model.bio,
    }))
}

#[utoipa::path(
    patch,
    path = "/users/me",
    request_body = UpdateUserRequestBody,
    responses(
        (status = 200, description = "User profile updated successfully", body = UserResponse),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 404, description = "User profile not found"),
        (status = 500, description = "Server error: failed to update profile")
    ),
    security(
        ("bearer_auth" = [])
    ),
    tag = "Users"
)]
pub async fn update_me(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
    Json(payload): Json<UpdateUserRequestBody>,
) -> Result<Json<UserResponse>, (StatusCode, String)> {
    let user_id = auth.user_id;

    let model = User::find_by_id(user_id)
        .one(&db)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Unable to retrieve your profile. Please try again.".to_string()))?;

    let Some(model) = model else {
        return Err((StatusCode::NOT_FOUND, "Your profile could not be found.".to_string()));
    };

    let mut active: UserActiveModel = model.into();

    if let Some(username) = payload.username {
        active.username = Set(username);
    }
    if let Some(avatar_url) = payload.avatar_url {
        active.avatar_url = Set(Some(avatar_url));
    }
    if let Some(bio) = payload.bio {
        active.bio = Set(Some(bio));
    }

    let model = active
        .update(&db)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to update profile. Please try again.".to_string()))?;

    Ok(Json(UserResponse {
        id: model.id,
        username: model.username,
        avatar_url: model.avatar_url,
        bio: model.bio,
    }))
}

#[utoipa::path(
    get,
    path = "/users/{id}",
    params(
        ("id" = Uuid, Path, description = "User ID")
    ),
    responses(
        (status = 200, description = "User profile retrieved successfully", body = UserResponse),
        (status = 404, description = "User not found: no user with the specified ID exists")
    ),
    tag = "Users"
)]
pub async fn get_user_by_id(
    State(db): State<DatabaseConnection>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Json<UserResponse>, (StatusCode, String)> {
    let model = User::find_by_id(id)
        .one(&db)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Unable to retrieve user profile. Please try again.".to_string()))?;

    let Some(model) = model else {
        return Err((StatusCode::NOT_FOUND, "This user profile does not exist.".to_string()));
    };

    Ok(Json(UserResponse {
        id: model.id,
        username: model.username,
        avatar_url: model.avatar_url,
        bio: model.bio,
    }))
}

#[utoipa::path(
    get,
    path = "/users/search",
    params(UserNameSearchQuery),
    responses(
        (status = 200, description = "Users found by username prefix", body = [UserResponse]),
        (status = 400, description = "Validation error: username query parameter is required"),
        (status = 500, description = "Server error: failed to search users")
    ),
    tag = "Users"
)]
pub async fn search_users(
    State(db): State<DatabaseConnection>,
    Query(query): Query<UserNameSearchQuery>,
) -> Result<Json<Vec<UserResponse>>, (StatusCode, String)> {
    let username = query.username.unwrap_or_default();
    if username.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Please enter a username to search.".to_string(),
        ));
    }

    let models = User::find()
        .filter(UserColumn::Username.starts_with(username))
        .all(&db)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to search users. Please try again.".to_string()))?;

    let users = models
        .into_iter()
        .map(|model| UserResponse {
            id: model.id,
            username: model.username,
            avatar_url: model.avatar_url,
            bio: model.bio,
        })
        .collect();

    Ok(Json(users))
}
