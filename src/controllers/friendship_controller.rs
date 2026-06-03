use crate::auth::middleware::AuthUser;
use crate::controllers::models::{FriendIdBody, UserDTO};
use crate::entities::friendship::FriendshipStatus;
use crate::entities::{Friendship, User, UserColumn, friendship, user};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, IntoActiveModel,
    QueryFilter, Set,
};
use uuid::Uuid;

pub use friendship::ActiveModel as FriendshipActive;
pub use friendship::Column as FriendshipColumn;

pub fn router() -> Router<DatabaseConnection> {
    Router::new()
        .route("/friends", get(get_friends))
        .route("/friends/request", post(friend_request))
        .route("/friends/incoming", get(get_incoming))
        .route("/friends/outgoing", get(get_outgoing))
        .route("/friends/{id}/accept", post(accept_friend_request))
        .route("/friends/{id}/remove", delete(remove_friend))
        .route("/friends/{id}/reject", post(reject_friend_request))
}

#[utoipa::path(
    get,
    path = "/friends",
    responses(
        (status = 200, description = "List of accepted friends retrieved successfully", body = [UserDTO]),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 500, description = "Server error: failed to retrieve friends list")
    ),
    security(
        ("bearer_auth" = [])
    ),
    tag = "Friends"
)]
pub async fn get_friends(
    auth: AuthUser,
    State(db_connection): State<DatabaseConnection>,
) -> Result<Json<Vec<UserDTO>>, (StatusCode, String)> {
    let me = auth.user_id;
    let rows = Friendship::find()
        .filter(friendship::Column::Status.eq(FriendshipStatus::Accepted))
        .filter(
            Condition::any()
                .add(FriendshipColumn::UserId.eq(me))
                .add(FriendshipColumn::FriendId.eq(me)),
        )
        .all(&db_connection)
        .await
        .map_err(internal_error)?;

    let ids: Vec<Uuid> = rows
        .into_iter()
        .map(|row| {
            if row.friend_id == me {
                row.user_id
            } else {
                row.friend_id
            }
        })
        .collect();

    if ids.is_empty() {
        return Ok(Json(vec![]));
    }

    let users = User::find()
        .filter(UserColumn::Id.is_in(ids))
        .all(&db_connection)
        .await
        .map_err(internal_error)?;

    Ok(Json(
        users.into_iter().map(|model| to_user_dto(&model)).collect(),
    ))
}

#[utoipa::path(
    get,
    path = "/friends/incoming",
    responses(
        (status = 200, description = "List of incoming friend requests retrieved successfully", body = [UserDTO]),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 500, description = "Server error: failed to retrieve incoming requests")
    ),
    security(
        ("bearer_auth" = [])
    ),
    tag = "Friends"
)]
pub async fn get_incoming(
    auth: AuthUser,
    State(db_connection): State<DatabaseConnection>,
) -> Result<Json<Vec<UserDTO>>, (StatusCode, String)> {
    let me = auth.user_id;

    let rows = Friendship::find()
        .filter(FriendshipColumn::FriendId.eq(me))
        .filter(FriendshipColumn::Status.eq(FriendshipStatus::Pending))
        .all(&db_connection)
        .await
        .map_err(internal_error)?;

    let ids: Vec<Uuid> = rows.into_iter().map(|row| row.user_id).collect();
    if ids.is_empty() {
        return Ok(Json(vec![]));
    }

    let users = User::find()
        .filter(UserColumn::Id.is_in(ids))
        .all(&db_connection)
        .await
        .map_err(internal_error)?;

    Ok(Json(
        users.into_iter().map(|model| to_user_dto(&model)).collect(),
    ))
}

#[utoipa::path(
    get,
    path = "/friends/outgoing",
    responses(
        (status = 200, description = "List of outgoing friend requests retrieved successfully", body = [UserDTO]),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 500, description = "Server error: failed to retrieve outgoing requests")
    ),
    security(
        ("bearer_auth" = [])
    ),
    tag = "Friends"
)]
pub async fn get_outgoing(
    auth: AuthUser,
    State(db_connection): State<DatabaseConnection>,
) -> Result<Json<Vec<UserDTO>>, (StatusCode, String)> {
    let me = auth.user_id;

    let rows = Friendship::find()
        .filter(FriendshipColumn::UserId.eq(me))
        .filter(FriendshipColumn::Status.eq(FriendshipStatus::Pending))
        .all(&db_connection)
        .await
        .map_err(internal_error)?;

    let ids: Vec<Uuid> = rows.into_iter().map(|row| row.friend_id).collect();
    if ids.is_empty() {
        return Ok(Json(vec![]));
    }

    let users = User::find()
        .filter(UserColumn::Id.is_in(ids))
        .all(&db_connection)
        .await
        .map_err(internal_error)?;

    Ok(Json(
        users.into_iter().map(|model| to_user_dto(&model)).collect(),
    ))
}

#[utoipa::path(
    post,
    path = "/friends/request",
    request_body = FriendIdBody,
    responses(
        (status = 201, description = "Friend request sent successfully"),
        (status = 400, description = "Validation error: cannot add yourself as a friend"),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 500, description = "Server error: failed to create friend request")
    ),
    security(
        ("bearer_auth" = [])
    ),
    tag = "Friends"
)]
pub async fn friend_request(
    auth: AuthUser,
    State(db_connection): State<DatabaseConnection>,
    Json(body): Json<FriendIdBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let me = auth.user_id;
    if body.friend_id == me {
        return Err((StatusCode::BAD_REQUEST, "You cannot add yourself as a friend.".to_string()));
    }

    let frindship_active = FriendshipActive {
        user_id: Set(me),
        friend_id: Set(body.friend_id),
        status: Set(FriendshipStatus::Pending),
        ..Default::default()
    };

    frindship_active
        .insert(&db_connection)
        .await
        .map_err(internal_error)?;
    Ok(StatusCode::CREATED)
}

#[utoipa::path(
    delete,
    path = "/friends/{id}/remove",
    params(
        ("id" = Uuid, Path, description = "Friend user ID")
    ),
    responses(
        (status = 204, description = "Friend removed successfully"),
        (status = 400, description = "Validation error: cannot remove yourself as a friend"),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 404, description = "Friendship not found: no accepted friendship exists with this user"),
        (status = 500, description = "Server error: failed to remove friend")
    ),
    security(
        ("bearer_auth" = [])
    ),
    tag = "Friends"
)]
pub async fn remove_friend(
    auth: AuthUser,
    State(db_connection): State<DatabaseConnection>,
    Path(friend_id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let me = auth.user_id;
    if me == friend_id {
        return Err((
            StatusCode::BAD_REQUEST,
            "You cannot remove yourself.".to_string(),
        ));
    }
    let result = Friendship::delete_many()
        .filter(FriendshipColumn::Status.eq(FriendshipStatus::Accepted))
        .filter(
            Condition::any()
                .add(
                    Condition::all()
                        .add(FriendshipColumn::UserId.eq(me))
                        .add(FriendshipColumn::FriendId.eq(friend_id)),
                )
                .add(
                    Condition::all()
                        .add(FriendshipColumn::UserId.eq(friend_id))
                        .add(FriendshipColumn::FriendId.eq(me)),
                ),
        )
        .exec(&db_connection)
        .await
        .map_err(internal_error)?;
    if result.rows_affected == 0 {
        return Err((StatusCode::NOT_FOUND, "This friendship does not exist.".to_string()));
    };

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/friends/{id}/accept",
    params(
        ("id" = Uuid, Path, description = "Sender user ID")
    ),
    responses(
        (status = 204, description = "Friend request accepted successfully"),
        (status = 400, description = "Validation error: cannot accept request from yourself"),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 404, description = "Request not found: no pending friend request from this user"),
        (status = 500, description = "Server error: failed to accept friend request")
    ),
    security(
        ("bearer_auth" = [])
    ),
    tag = "Friends"
)]
pub async fn accept_friend_request(
    auth: AuthUser,
    State(db_connection): State<DatabaseConnection>,
    Path(sender_user_id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let me = auth.user_id;

    if sender_user_id == me {
        return Err((
            StatusCode::BAD_REQUEST,
            "You cannot accept your own request.".to_string(),
        ));
    }

    let row = Friendship::find()
        .filter(FriendshipColumn::UserId.eq(sender_user_id))
        .filter(FriendshipColumn::FriendId.eq(me))
        .filter(FriendshipColumn::Status.eq(FriendshipStatus::Pending))
        .one(&db_connection)
        .await
        .map_err(internal_error)?;

    let Some(row) = row else {
        return Err((StatusCode::NOT_FOUND, "This friend request does not exist.".to_string()));
    };

    let mut active = row.into_active_model();
    active.status = Set(FriendshipStatus::Accepted);

    active
        .update(&db_connection)
        .await
        .map_err(internal_error)?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/friends/{id}/reject",
    params(
        ("id" = Uuid, Path, description = "Sender user ID")
    ),
    responses(
        (status = 204, description = "Friend request rejected successfully"),
        (status = 400, description = "Validation error: cannot reject request from yourself"),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 404, description = "Request not found: no pending friend request from this user"),
        (status = 500, description = "Server error: failed to reject friend request")
    ),
    security(
        ("bearer_auth" = [])
    ),
    tag = "Friends"
)]
pub async fn reject_friend_request(
    auth: AuthUser,
    State(db_connection): State<DatabaseConnection>,
    Path(sender_user_id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let me = auth.user_id;
    if sender_user_id == me {
        return Err((
            StatusCode::BAD_REQUEST,
            "You cannot reject your own request.".to_string(),
        ));
    }

    let row = Friendship::find()
        .filter(FriendshipColumn::UserId.eq(sender_user_id))
        .filter(FriendshipColumn::FriendId.eq(me))
        .filter(FriendshipColumn::Status.eq(FriendshipStatus::Pending))
        .one(&db_connection)
        .await
        .map_err(internal_error)?;

    let Some(row) = row else {
        return Err((StatusCode::NOT_FOUND, "This friend request does not exist.".to_string()));
    };

    row.into_active_model()
        .delete(&db_connection)
        .await
        .map_err(internal_error)?;

    Ok(StatusCode::NO_CONTENT)
}

// MARK: Helper methods
fn internal_error<E: std::fmt::Display>(_e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong. Please try again.".to_string())
}

fn to_user_dto(u: &user::Model) -> UserDTO {
    UserDTO {
        id: u.id,
        username: u.username.clone(),
        avatar_url: u.avatar_url.clone(),
        bio: u.bio.clone(),
    }
}
