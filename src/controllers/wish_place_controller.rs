use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, IntoActiveModel,
    QueryFilter, QueryOrder, Set,
};
use uuid::Uuid;

use crate::auth::middleware::AuthUser;
use crate::controllers::models::wish_place::{
    CreateWishPlaceBody, UpdateWishPlaceBody, VisitWishPlaceBody, WishPlaceQuery,
    WishPlaceResponse, WishPlaceStatusDto,
};
use crate::entities::friendship::{self, FriendshipStatus};
use crate::entities::wish_place::{self, WishPlaceStatus};
use crate::entities::{Event, Friendship, WishPlace, WishPlaceActiveModel, WishPlaceColumn};

pub fn router() -> Router<DatabaseConnection> {
    Router::new()
        .route("/wish-places", get(get_wish_places).post(create_wish_place))
        .route(
            "/wish-places/{id}",
            patch(update_wish_place).delete(delete_wish_place),
        )
        .route("/wish-places/{id}/visit", post(visit_wish_place))
}

#[utoipa::path(
    get,
    path = "/wish-places",
    summary = "Get wish places",
    description = "Returns wish places for the specified user. Access: self or accepted friend only.",
    params(WishPlaceQuery),
    responses(
        (status = 200, description = "Wish places list retrieved successfully", body = [WishPlaceResponse]),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 403, description = "Forbidden: you can only view your own or accepted friend's wish places"),
        (status = 500, description = "Server error: failed to retrieve wish places")
    ),
    security(("bearer_auth" = [])),
    tag = "WishPlaces"
)]
pub async fn get_wish_places(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
    Query(query): Query<WishPlaceQuery>,
) -> Result<Json<Vec<WishPlaceResponse>>, (StatusCode, String)> {
    let me = auth.user_id;
    if me != query.user_id && !are_users_accepted_friends(&db, me, query.user_id).await? {
        return Err((StatusCode::FORBIDDEN, "You can only view your own or accepted friend's wish places.".to_string()));
    }

    let rows = WishPlace::find()
        .filter(WishPlaceColumn::UserId.eq(query.user_id))
        .order_by_desc(WishPlaceColumn::CreatedAt)
        .all(&db)
        .await
        .map_err(internal_error)?;

    Ok(Json(rows.into_iter().map(to_response).collect()))
}

#[utoipa::path(
    post,
    path = "/wish-places",
    summary = "Create wish place",
    description = "Creates a new place in current user's wish list.",
    request_body = CreateWishPlaceBody,
    responses(
        (status = 201, description = "Wish place created successfully", body = WishPlaceResponse),
        (status = 400, description = "Validation error: title is required"),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 500, description = "Server error: failed to create wish place")
    ),
    security(("bearer_auth" = [])),
    tag = "WishPlaces"
)]
pub async fn create_wish_place(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
    Json(body): Json<CreateWishPlaceBody>,
) -> Result<(StatusCode, Json<WishPlaceResponse>), (StatusCode, String)> {
    if body.title.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Please enter a title for the wish place.".to_string()));
    }

    let model = WishPlaceActiveModel {
        user_id: Set(auth.user_id),
        title: Set(body.title),
        description: Set(body.description),
        location: Set(body.location),
        link: Set(body.link),
        status: Set(WishPlaceStatus::Active),
        visited_event_id: Set(None),
        ..Default::default()
    }
    .insert(&db)
    .await
    .map_err(internal_error)?;

    Ok((StatusCode::CREATED, Json(to_response(model))))
}

#[utoipa::path(
    patch,
    path = "/wish-places/{id}",
    summary = "Update wish place",
    description = "Updates wish place details. Owner only.",
    request_body = UpdateWishPlaceBody,
    params(("id" = Uuid, Path, description = "Wish place ID")),
    responses(
        (status = 200, description = "Wish place updated successfully", body = WishPlaceResponse),
        (status = 400, description = "Validation error: title cannot be empty or nothing to update"),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 404, description = "Wish place not found or you are not the owner"),
        (status = 500, description = "Server error: failed to update wish place")
    ),
    security(("bearer_auth" = [])),
    tag = "WishPlaces"
)]
pub async fn update_wish_place(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateWishPlaceBody>,
) -> Result<Json<WishPlaceResponse>, (StatusCode, String)> {
    let row = WishPlace::find_by_id(id)
        .filter(WishPlaceColumn::UserId.eq(auth.user_id))
        .one(&db)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Wish place not found or you are not the owner.".to_string()))?;

    if body.title.is_none()
        && body.description.is_none()
        && body.location.is_none()
        && body.link.is_none()
        && body.status.is_none()
    {
        return Err((StatusCode::BAD_REQUEST, "Please provide at least one field to update.".to_string()));
    }

    let mut active = row.into_active_model();

    if let Some(title) = body.title {
        if title.trim().is_empty() {
            return Err((StatusCode::BAD_REQUEST, "Title cannot be empty.".to_string()));
        }
        active.title = Set(title);
    }
    if let Some(description) = body.description {
        active.description = Set(Some(description));
    }
    if let Some(location) = body.location {
        active.location = Set(Some(location));
    }
    if let Some(link) = body.link {
        active.link = Set(Some(link));
    }
    if let Some(status) = body.status {
        active.status = Set(map_status(status));
        if !matches!(status, WishPlaceStatusDto::Visited) {
            active.visited_event_id = Set(None);
        }
    }

    let updated = active.update(&db).await.map_err(internal_error)?;
    Ok(Json(to_response(updated)))
}

#[utoipa::path(
    post,
    path = "/wish-places/{id}/visit",
    summary = "Mark wish place as visited",
    description = "Marks wish place as visited and associates with event. Owner and event creator only.",
    request_body = VisitWishPlaceBody,
    params(("id" = Uuid, Path, description = "Wish place ID")),
    responses(
        (status = 200, description = "Wish place marked as visited", body = WishPlaceResponse),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 403, description = "Forbidden: only event creator can mark place as visited"),
        (status = 404, description = "Wish place or event not found"),
        (status = 500, description = "Server error: failed to mark wish place as visited")
    ),
    security(("bearer_auth" = [])),
    tag = "WishPlaces"
)]
pub async fn visit_wish_place(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
    Path(id): Path<Uuid>,
    Json(body): Json<VisitWishPlaceBody>,
) -> Result<Json<WishPlaceResponse>, (StatusCode, String)> {
    let row = WishPlace::find_by_id(id)
        .filter(WishPlaceColumn::UserId.eq(auth.user_id))
        .one(&db)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Wish place not found or you are not the owner.".to_string()))?;

    let event = Event::find_by_id(body.event_id)
        .one(&db)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Event not found.".to_string()))?;

    if event.creator_id != auth.user_id {
        return Err((
            StatusCode::FORBIDDEN,
            "Only the event creator can mark a place as visited.".to_string(),
        ));
    }

    let mut active = row.into_active_model();
    active.status = Set(WishPlaceStatus::Visited);
    active.visited_event_id = Set(Some(body.event_id));

    let updated = active.update(&db).await.map_err(internal_error)?;
    Ok(Json(to_response(updated)))
}

#[utoipa::path(
    delete,
    path = "/wish-places/{id}",
    summary = "Archive wish place",
    description = "Archives wish place (status=archived). Owner only.",
    params(("id" = Uuid, Path, description = "Wish place ID")),
    responses(
        (status = 204, description = "Wish place archived successfully"),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 404, description = "Wish place not found or you are not the owner"),
        (status = 500, description = "Server error: failed to archive wish place")
    ),
    security(("bearer_auth" = [])),
    tag = "WishPlaces"
)]
pub async fn delete_wish_place(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let row = WishPlace::find_by_id(id)
        .filter(WishPlaceColumn::UserId.eq(auth.user_id))
        .one(&db)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Wish place not found or you are not the owner.".to_string()))?;

    let mut active = row.into_active_model();
    active.status = Set(WishPlaceStatus::Archived);
    active.update(&db).await.map_err(internal_error)?;

    Ok(StatusCode::NO_CONTENT)
}

fn to_response(model: wish_place::Model) -> WishPlaceResponse {
    WishPlaceResponse {
        id: model.id,
        user_id: model.user_id,
        title: model.title,
        description: model.description,
        location: model.location,
        link: model.link,
        status: model.status.to_string(),
        visited_event_id: model.visited_event_id,
        created_at: model.created_at.to_rfc3339(),
    }
}

fn map_status(value: WishPlaceStatusDto) -> WishPlaceStatus {
    match value {
        WishPlaceStatusDto::Active => WishPlaceStatus::Active,
        WishPlaceStatusDto::Visited => WishPlaceStatus::Visited,
        WishPlaceStatusDto::Archived => WishPlaceStatus::Archived,
    }
}

async fn are_users_accepted_friends(
    db: &DatabaseConnection,
    user_a: Uuid,
    user_b: Uuid,
) -> Result<bool, (StatusCode, String)> {
    let row = Friendship::find()
        .filter(friendship::Column::Status.eq(FriendshipStatus::Accepted))
        .filter(
            Condition::any()
                .add(
                    Condition::all()
                        .add(friendship::Column::UserId.eq(user_a))
                        .add(friendship::Column::FriendId.eq(user_b)),
                )
                .add(
                    Condition::all()
                        .add(friendship::Column::UserId.eq(user_b))
                        .add(friendship::Column::FriendId.eq(user_a)),
                ),
        )
        .one(db)
        .await
        .map_err(internal_error)?;
    Ok(row.is_some())
}

fn internal_error<E: std::fmt::Display>(_e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong. Please try again.".to_string())
}
