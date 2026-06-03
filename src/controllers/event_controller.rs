use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use chrono::{NaiveDate, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, EntityTrait,
    IntoActiveModel, PaginatorTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};
use uuid::Uuid;

use crate::auth::middleware::AuthUser;
use crate::controllers::models::events::{
    CreateEventBody, EventResponse, EventScope, EventScopeQuery, FinishEventBody,
    ParticipantResponse, UserAvailabilityResponse,
};
use crate::entities::event::EventStatus;
use crate::entities::friendship::{self, FriendshipStatus};
use crate::entities::user_event::{UserEventResponse, UserEventRole};
use crate::entities::{
    Busyday, BusydayActiveModel, BusydayColumn, Event, EventActiveModel, EventColumn, Friendship,
    UserEvent, UserEventActiveModel, UserEventColumn, event,
};

pub fn router() -> Router<DatabaseConnection> {
    Router::new()
        .route("/events", post(create_event).get(get_events))
        .route("/events/active", get(get_active_events))
        .route("/events/pending", get(get_pending_events))
        .route("/events/waiting", get(get_waiting_events))
        .route("/events/check-user-availability", get(check_user_availability))
        .route("/events/check-availability", get(check_friends_availability))
        .route("/events/{id}", get(get_event))
        .route("/events/{id}/finish", post(finish_event))
        .route("/events/{id}/cancel", post(cancel_event))
        .route("/events/{id}/participants", get(get_event_participants))
        .route("/events/{id}/accept", post(accept_event))
        .route("/events/{id}/decline", post(decline_event))
}

#[utoipa::path(
    post,
    path = "/events",
    summary = "Create event",
    description = "Creates an event for a single day. Current user becomes the event owner with accepted status. Participants are added with pending status. Can only invite accepted friends.",
    request_body = CreateEventBody,
    responses(
        (status = 201, description = "Event created successfully", body = EventResponse),
        (status = 400, description = "Validation error: title is required"),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 403, description = "Forbidden: can invite only accepted friends"),
        (status = 409, description = "Conflict: one or more participants are busy on the selected date"),
        (status = 500, description = "Server error: failed to create event")
    ),
    security(("bearer_auth" = [])),
    tag = "Events"
)]
pub async fn create_event(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
    Json(body): Json<CreateEventBody>,
) -> Result<(StatusCode, Json<EventResponse>), (StatusCode, String)> {
    let me = auth.user_id;
    let date = parse_date(&body.date)?;

    if body.title.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Please enter an event title.".to_string()));
    }

    let mut participant_ids = body.participant_ids;
    participant_ids.retain(|id| *id != me);
    participant_ids.sort();
    participant_ids.dedup();

    for participant_id in &participant_ids {
        if !are_users_accepted_friends(&db, me, *participant_id).await? {
            return Err((
                StatusCode::FORBIDDEN,
                "You can only invite accepted friends to events.".to_string(),
            ));
        }
    }

    let tx = db.begin().await.map_err(internal_error)?;
    ensure_day_is_free(&tx, me, date).await?;
    for participant_id in &participant_ids {
        if let Err((status, message)) = ensure_day_is_free(&tx, *participant_id, date).await {
            if status == StatusCode::CONFLICT {
                return Err((
                    StatusCode::CONFLICT,
                    "One or more participants are already busy on this date.".to_string(),
                ));
            }
            return Err((status, message));
        }
    }

    let event = EventActiveModel {
        creator_id: Set(me),
        date: Set(date),
        title: Set(body.title),
        description: Set(body.description),
        location: Set(body.location),
        status: Set(EventStatus::Pending),
        wish_place_id: Set(body.wish_place_id),
        memory_image_base64: Set(None),
        ..Default::default()
    }
    .insert(&tx)
    .await
    .map_err(internal_error)?;

    UserEventActiveModel {
        event_id: Set(event.id),
        user_id: Set(me),
        role: Set(UserEventRole::Owner),
        response_status: Set(UserEventResponse::Accepted),
        ..Default::default()
    }
    .insert(&tx)
    .await
    .map_err(internal_error)?;

    BusydayActiveModel {
        user_id: Set(me),
        date: Set(date),
        event_id: Set(Some(event.id)),
        ..Default::default()
    }
    .insert(&tx)
    .await
    .map_err(map_db_constraint_error)?;

    if !participant_ids.is_empty() {
        let models = participant_ids
            .into_iter()
            .map(|participant_id| UserEventActiveModel {
                event_id: Set(event.id),
                user_id: Set(participant_id),
                role: Set(UserEventRole::Participant),
                response_status: Set(UserEventResponse::Pending),
                ..Default::default()
            })
            .collect::<Vec<_>>();

        UserEvent::insert_many(models)
            .exec(&tx)
            .await
            .map_err(internal_error)?;
    }

    tx.commit().await.map_err(internal_error)?;

    let response = load_event_response(&db, event.id).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/events/{id}",
    summary = "Get event details",
    description = "Returns event data with participants. Access restricted to event participants only.",
    params(("id" = Uuid, Path, description = "Event ID")),
    responses(
        (status = 200, description = "Event details retrieved successfully", body = EventResponse),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 403, description = "Forbidden: you are not a participant in this event"),
        (status = 404, description = "Event not found")
    ),
    security(("bearer_auth" = [])),
    tag = "Events"
)]
pub async fn get_event(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
    Path(id): Path<Uuid>,
) -> Result<Json<EventResponse>, (StatusCode, String)> {
    let me = auth.user_id;
    ensure_event_access(&db, id, me).await?;
    let event = load_event_response(&db, id).await.map_err(|_| (StatusCode::NOT_FOUND, "Event not found.".to_string()))?;
    Ok(Json(event))
}

#[utoipa::path(
    get,
    path = "/events",
    summary = "List events",
    description = "Returns events for the current user filtered by scope (created, invited, upcoming, past).",
    params(EventScopeQuery),
    responses(
        (status = 200, description = "Events list retrieved successfully", body = [EventResponse]),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 500, description = "Server error: failed to retrieve events")
    ),
    security(("bearer_auth" = [])),
    tag = "Events"
)]
pub async fn get_events(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
    Query(query): Query<EventScopeQuery>,
) -> Result<Json<Vec<EventResponse>>, (StatusCode, String)> {
    let me = auth.user_id;
    let scope = query.scope.unwrap_or(EventScope::Upcoming);
    let today = Utc::now().date_naive();

    let events = match scope {
        EventScope::Created => Event::find()
            .filter(EventColumn::CreatorId.eq(me))
            .order_by_asc(EventColumn::Date)
            .all(&db)
            .await
            .map_err(internal_error)?,
        EventScope::Invited => {
            let event_ids = UserEvent::find()
                .filter(UserEventColumn::UserId.eq(me))
                .filter(UserEventColumn::Role.eq(UserEventRole::Participant))
                .filter(UserEventColumn::ResponseStatus.ne(UserEventResponse::Declined))
                .all(&db)
                .await
                .map_err(internal_error)?
                .into_iter()
                .map(|row| row.event_id)
                .collect::<Vec<_>>();

            if event_ids.is_empty() {
                Vec::new()
            } else {
                Event::find()
                    .filter(EventColumn::Id.is_in(event_ids))
                    .order_by_asc(EventColumn::Date)
                    .all(&db)
                    .await
                    .map_err(internal_error)?
            }
        }
        EventScope::Upcoming => {
            let event_ids = accepted_event_ids(&db, me).await?;
            if event_ids.is_empty() {
                Vec::new()
            } else {
                Event::find()
                    .filter(EventColumn::Id.is_in(event_ids))
                    .filter(EventColumn::Date.gte(today))
                    .filter(EventColumn::Status.ne(EventStatus::Canceled))
                    .order_by_asc(EventColumn::Date)
                    .all(&db)
                    .await
                    .map_err(internal_error)?
            }
        }
        EventScope::Past => {
            let event_ids = accepted_event_ids(&db, me).await?;
            if event_ids.is_empty() {
                Vec::new()
            } else {
                Event::find()
                    .filter(EventColumn::Id.is_in(event_ids))
                    .filter(EventColumn::Date.lt(today))
                    .order_by_desc(EventColumn::Date)
                    .all(&db)
                    .await
                    .map_err(internal_error)?
            }
        }
    };

    let mut response = Vec::with_capacity(events.len());
    for row in events {
        response.push(load_event_response(&db, row.id).await?);
    }

    Ok(Json(response))
}

#[utoipa::path(
    get,
    path = "/events/active",
    summary = "Get active events",
    description = "Returns events where all participants (including creator) have accepted status.",
    responses(
        (status = 200, description = "Active events retrieved successfully", body = [EventResponse]),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 500, description = "Server error: failed to retrieve active events")
    ),
    security(("bearer_auth" = [])),
    tag = "Events"
)]
pub async fn get_active_events(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
) -> Result<Json<Vec<EventResponse>>, (StatusCode, String)> {
    let me = auth.user_id;
    let today = Utc::now().date_naive();

    let user_event_ids = UserEvent::find()
        .filter(
            Condition::any()
                .add(UserEventColumn::UserId.eq(me))
        )
        .filter(UserEventColumn::ResponseStatus.ne(UserEventResponse::Declined))
        .all(&db)
        .await
        .map_err(internal_error)?
        .into_iter()
        .map(|row| row.event_id)
        .collect::<Vec<_>>();

    if user_event_ids.is_empty() {
        return Ok(Json(Vec::new()));
    }

    let events = Event::find()
        .filter(EventColumn::Id.is_in(user_event_ids))
        .all(&db)
        .await
        .map_err(internal_error)?;

    let mut response = Vec::new();
    for event in events {
        if event.date < today {
            let participants = load_participants(&db, event.id).await?;
            let all_accepted = participants
                .iter()
                .all(|p| p.response_status == "accepted");

            let new_status = if all_accepted {
                EventStatus::Completed
            } else {
                EventStatus::Canceled
            };

            let mut active = event.into_active_model();
            active.status = Set(new_status);
            active.update(&db).await.map_err(internal_error)?;

            continue;
        }

        let participants = load_participants(&db, event.id).await?;
        let all_accepted = participants
            .iter()
            .all(|p| p.response_status == "accepted");

        if all_accepted {
            response.push(load_event_response(&db, event.id).await?);
        }
    }

    Ok(Json(response))
}

#[utoipa::path(
    get,
    path = "/events/pending",
    summary = "Get pending events",
    description = "Returns events where current user has accepted and event status is pending (waiting for others).",
    responses(
        (status = 200, description = "Pending events retrieved successfully", body = [EventResponse]),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 500, description = "Server error: failed to retrieve pending events")
    ),
    security(("bearer_auth" = [])),
    tag = "Events"
)]
pub async fn get_pending_events(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
) -> Result<Json<Vec<EventResponse>>, (StatusCode, String)> {
    let me = auth.user_id;

    // Find all events where:
    // 1. Current user is a participant with response_status == ACCEPTED
    // 2. Event status == PENDING
    let user_events = UserEvent::find()
        .filter(UserEventColumn::UserId.eq(me))
        .filter(UserEventColumn::ResponseStatus.eq(UserEventResponse::Accepted))
        .all(&db)
        .await
        .map_err(internal_error)?;

    if user_events.is_empty() {
        return Ok(Json(Vec::new()));
    }

    let event_ids = user_events
        .into_iter()
        .map(|row| row.event_id)
        .collect::<Vec<_>>();

    let events = Event::find()
        .filter(EventColumn::Id.is_in(event_ids))
        .filter(EventColumn::Status.eq(EventStatus::Pending))
        .order_by_asc(EventColumn::Date)
        .all(&db)
        .await
        .map_err(internal_error)?;

    let mut response = Vec::with_capacity(events.len());
    for event in events {
        response.push(load_event_response(&db, event.id).await?);
    }

    Ok(Json(response))
}

#[utoipa::path(
    get,
    path = "/events/waiting",
    summary = "Get events awaiting response",
    description = "Returns events where current user is a participant and has not yet accepted the invitation.",
    responses(
        (status = 200, description = "Events awaiting response retrieved successfully", body = [EventResponse]),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 500, description = "Server error: failed to retrieve events")
    ),
    security(("bearer_auth" = [])),
    tag = "Events"
)]
pub async fn get_waiting_events(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
) -> Result<Json<Vec<EventResponse>>, (StatusCode, String)> {
    let me = auth.user_id;
    let today = Utc::now().date_naive();

    // Find all user_events where:
    // 1. user_id == me
    // 2. role == Participant
    // 3. response_status != Accepted AND != Declined
    let user_events = UserEvent::find()
        .filter(UserEventColumn::UserId.eq(me))
        .filter(UserEventColumn::Role.eq(UserEventRole::Participant))
        .filter(UserEventColumn::ResponseStatus.ne(UserEventResponse::Accepted))
        .filter(UserEventColumn::ResponseStatus.ne(UserEventResponse::Declined))
        .all(&db)
        .await
        .map_err(internal_error)?;

    if user_events.is_empty() {
        return Ok(Json(Vec::new()));
    }

    let event_ids = user_events
        .into_iter()
        .map(|row| row.event_id)
        .collect::<Vec<_>>();

    // Load events and filter out past events
    let events = Event::find()
        .filter(EventColumn::Id.is_in(event_ids))
        .filter(EventColumn::Date.gte(today))
        .filter(EventColumn::Status.ne(EventStatus::Canceled))
        .filter(EventColumn::Status.ne(EventStatus::Completed))
        .order_by_asc(EventColumn::Date)
        .all(&db)
        .await
        .map_err(internal_error)?;

    let mut response = Vec::with_capacity(events.len());
    for event in events {
        response.push(load_event_response(&db, event.id).await?);
    }

    Ok(Json(response))
}

#[derive(serde::Deserialize, utoipa::IntoParams)]
pub struct CheckAvailabilityQuery {
    date: String,
}

#[utoipa::path(
    get,
    path = "/events/check-user-availability",
    summary = "Check current user availability",
    description = "Checks if current user is available on the specified date.",
    params(CheckAvailabilityQuery),
    responses(
        (status = 200, description = "User availability status retrieved", body = UserAvailabilityResponse),
        (status = 400, description = "Validation error: invalid date format (use YYYY-MM-DD)"),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 500, description = "Server error: failed to check availability")
    ),
    security(("bearer_auth" = [])),
    tag = "Events"
)]
pub async fn check_user_availability(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
    Query(q): Query<CheckAvailabilityQuery>,
) -> Result<Json<UserAvailabilityResponse>, (StatusCode, String)> {
    let me = auth.user_id;
    let date = parse_date(&q.date)?;

    let is_busy = Busyday::find()
        .filter(BusydayColumn::UserId.eq(me))
        .filter(BusydayColumn::Date.eq(date))
        .one(&db)
        .await
        .map_err(internal_error)?
        .is_some();

    Ok(Json(UserAvailabilityResponse {
        is_available: !is_busy,
    }))
}

#[utoipa::path(
    get,
    path = "/events/check-availability",
    summary = "Check friends availability",
    description = "Returns list of friends available on the specified date.",
    params(CheckAvailabilityQuery),
    responses(
        (status = 200, description = "Available friends list retrieved", body = serde_json::Value),
        (status = 400, description = "Validation error: invalid date format (use YYYY-MM-DD)"),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 500, description = "Server error: failed to check friends availability")
    ),
    security(("bearer_auth" = [])),
    tag = "Events"
)]
pub async fn check_friends_availability(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
    Query(q): Query<CheckAvailabilityQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let me = auth.user_id;
    let date = parse_date(&q.date)?;

    let accepted_friends = Friendship::find()
        .filter(friendship::Column::Status.eq(FriendshipStatus::Accepted))
        .filter(
            Condition::any()
                .add(friendship::Column::UserId.eq(me))
                .add(friendship::Column::FriendId.eq(me)),
        )
        .all(&db)
        .await
        .map_err(internal_error)?;

    let friend_ids: Vec<Uuid> = accepted_friends
        .iter()
        .map(|f| if f.user_id == me { f.friend_id } else { f.user_id })
        .collect();

    if friend_ids.is_empty() {
        return Ok(Json(serde_json::json!({ "available_friends": [] })));
    }

    let busy_user_ids = Busyday::find()
        .filter(BusydayColumn::Date.eq(date))
        .filter(BusydayColumn::UserId.is_in(friend_ids.clone()))
        .all(&db)
        .await
        .map_err(internal_error)?
        .into_iter()
        .map(|b| b.user_id)
        .collect::<Vec<_>>();

    let available_friends = crate::entities::user::Entity::find()
        .filter(crate::entities::user::Column::Id.is_in(friend_ids))
        .filter(
            if busy_user_ids.is_empty() {
                Condition::all()
            } else {
                Condition::all().add(crate::entities::user::Column::Id.is_not_in(busy_user_ids))
            },
        )
        .order_by_asc(crate::entities::user::Column::Username)
        .all(&db)
        .await
        .map_err(internal_error)?;

    let response: Vec<serde_json::Value> = available_friends
        .into_iter()
        .map(|user| {
            serde_json::json!({
                "id": user.id,
                "username": user.username,
                "avatar_url": user.avatar_url,
                "bio": user.bio,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "available_friends": response })))
}

#[utoipa::path(
    post,
    path = "/events/{id}/finish",
    summary = "Complete event",
    description = "Marks event as completed with a memory image. Only creator can complete. Must be on or after event date.",
    request_body = FinishEventBody,
    params(("id" = Uuid, Path, description = "Event ID")),
    responses(
        (status = 200, description = "Event completed successfully", body = EventResponse),
        (status = 400, description = "Validation error: memory image cannot be empty"),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 404, description = "Event not found or you are not the creator"),
        (status = 409, description = "Conflict: event cannot be completed before event date or is already completed/canceled"),
        (status = 500, description = "Server error: failed to complete event")
    ),
    security(("bearer_auth" = [])),
    tag = "Events"
)]
pub async fn finish_event(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
    Path(id): Path<Uuid>,
    Json(body): Json<FinishEventBody>,
) -> Result<Json<EventResponse>, (StatusCode, String)> {
    let me = auth.user_id;
    let today = Utc::now().date_naive();

    let event = Event::find_by_id(id)
        .filter(EventColumn::CreatorId.eq(me))
        .one(&db)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Event not found or you are not the creator.".to_string()))?;

    if matches!(event.status, EventStatus::Canceled | EventStatus::Completed) {
        return Err((
            StatusCode::CONFLICT,
            "This event has already been completed or canceled.".to_string(),
        ));
    }

    if today < event.date {
        return Err((
            StatusCode::CONFLICT,
            "You can only complete an event on or after the event date.".to_string(),
        ));
    }

    if body.memory_image_base64.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Please add a memory image to complete the event.".to_string(),
        ));
    }

    let mut active = event.into_active_model();
    active.memory_image_base64 = Set(Some(body.memory_image_base64));
    active.status = Set(EventStatus::Completed);
    active.update(&db).await.map_err(internal_error)?;

    Ok(Json(load_event_response(&db, id).await?))
}

#[utoipa::path(
    post,
    path = "/events/{id}/cancel",
    summary = "Cancel event",
    description = "Cancels and deletes event. Only creator can cancel. Removes all participant reservations.",
    params(("id" = Uuid, Path, description = "Event ID")),
    responses(
        (status = 204, description = "Event canceled successfully"),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 404, description = "Event not found or you are not the creator"),
        (status = 409, description = "Conflict: completed events cannot be canceled"),
        (status = 500, description = "Server error: failed to cancel event")
    ),
    security(("bearer_auth" = [])),
    tag = "Events"
)]
pub async fn cancel_event(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let me = auth.user_id;
    let tx = db.begin().await.map_err(internal_error)?;

    let event = Event::find_by_id(id)
        .filter(EventColumn::CreatorId.eq(me))
        .one(&tx)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "event not found".to_string()))?;

    if matches!(event.status, EventStatus::Completed) {
        return Err((
            StatusCode::CONFLICT,
            "Completed events cannot be canceled.".to_string(),
        ));
    }

    Busyday::delete_many()
        .filter(BusydayColumn::EventId.eq(id))
        .exec(&tx)
        .await
        .map_err(internal_error)?;

    UserEvent::delete_many()
        .filter(UserEventColumn::EventId.eq(id))
        .exec(&tx)
        .await
        .map_err(internal_error)?;

    event
        .into_active_model()
        .delete(&tx)
        .await
        .map_err(internal_error)?;

    tx.commit().await.map_err(internal_error)?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/events/{id}/participants",
    summary = "Get event participants",
    description = "Returns list of event participants with their response status.",
    params(("id" = Uuid, Path, description = "Event ID")),
    responses(
        (status = 200, description = "Participants list retrieved successfully", body = [ParticipantResponse]),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 403, description = "Forbidden: you are not a participant in this event"),
        (status = 404, description = "Event not found"),
        (status = 500, description = "Server error: failed to retrieve participants")
    ),
    security(("bearer_auth" = [])),
    tag = "Events"
)]
pub async fn get_event_participants(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ParticipantResponse>>, (StatusCode, String)> {
    let me = auth.user_id;
    ensure_event_access(&db, id, me).await?;

    let participants = load_participants(&db, id).await?;
    Ok(Json(participants))
}

#[utoipa::path(
    post,
    path = "/events/{id}/accept",
    summary = "Accept event invitation",
    description = "Accepts event invitation. Participant must have pending status. Reserves the date if all others accept.",
    params(("id" = Uuid, Path, description = "Event ID")),
    responses(
        (status = 200, description = "Invitation accepted successfully", body = EventResponse),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 404, description = "Event not found or you are not a pending participant"),
        (status = 409, description = "Conflict: selected day is already busy or invitation already responded"),
        (status = 500, description = "Server error: failed to accept invitation")
    ),
    security(("bearer_auth" = [])),
    tag = "Events"
)]
pub async fn accept_event(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
    Path(id): Path<Uuid>,
) -> Result<Json<EventResponse>, (StatusCode, String)> {
    let me = auth.user_id;
    let tx = db.begin().await.map_err(internal_error)?;

    let event = Event::find_by_id(id)
        .one(&tx)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Event not found.".to_string()))?;

    if matches!(event.status, EventStatus::Canceled | EventStatus::Completed) {
        return Err((
            StatusCode::CONFLICT,
            "This event has already been completed or canceled.".to_string(),
        ));
    }

    let participant = UserEvent::find()
        .filter(UserEventColumn::EventId.eq(id))
        .filter(UserEventColumn::UserId.eq(me))
        .filter(UserEventColumn::Role.eq(UserEventRole::Participant))
        .filter(UserEventColumn::ResponseStatus.eq(UserEventResponse::Pending))
        .one(&tx)
        .await
        .map_err(internal_error)?
        .ok_or((
            StatusCode::NOT_FOUND,
            "You are not a pending participant in this event.".to_string(),
        ))?;

    ensure_day_is_free(&tx, me, event.date).await?;

    let mut active = participant.into_active_model();
    active.response_status = Set(UserEventResponse::Accepted);
    active.update(&tx).await.map_err(internal_error)?;

    BusydayActiveModel {
        user_id: Set(me),
        date: Set(event.date),
        event_id: Set(Some(id)),
        ..Default::default()
    }
    .insert(&tx)
    .await
    .map_err(map_db_constraint_error)?;

    let non_accepted_exists = UserEvent::find()
        .filter(UserEventColumn::EventId.eq(id))
        .filter(UserEventColumn::Role.eq(UserEventRole::Participant))
        .filter(UserEventColumn::ResponseStatus.ne(UserEventResponse::Accepted))
        .one(&tx)
        .await
        .map_err(internal_error)?
        .is_some();

    if !non_accepted_exists {
        let mut event_active = event.into_active_model();
        event_active.status = Set(EventStatus::Confirmed);
        event_active.update(&tx).await.map_err(internal_error)?;
    }

    tx.commit().await.map_err(internal_error)?;

    Ok(Json(load_event_response(&db, id).await?))
}

#[utoipa::path(
    post,
    path = "/events/{id}/decline",
    summary = "Decline event invitation",
    description = "Declines event invitation. Frees the date. If this is the last participant, event is canceled.",
    params(("id" = Uuid, Path, description = "Event ID")),
    responses(
        (status = 204, description = "Invitation declined successfully"),
        (status = 401, description = "Unauthorized: invalid or missing authentication token"),
        (status = 404, description = "Event not found or you are not a participant"),
        (status = 409, description = "Conflict: event is already completed/canceled"),
        (status = 500, description = "Server error: failed to decline invitation")
    ),
    security(("bearer_auth" = [])),
    tag = "Events"
)]
pub async fn decline_event(
    auth: AuthUser,
    State(db): State<DatabaseConnection>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let me = auth.user_id;
    let tx = db.begin().await.map_err(internal_error)?;

    let event = Event::find_by_id(id)
        .one(&tx)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Event not found.".to_string()))?;

    if matches!(event.status, EventStatus::Canceled | EventStatus::Completed) {
        return Err((
            StatusCode::CONFLICT,
            "This event has already been completed or canceled.".to_string(),
        ));
    }

    let participant = UserEvent::find()
        .filter(UserEventColumn::EventId.eq(id))
        .filter(UserEventColumn::UserId.eq(me))
        .filter(UserEventColumn::Role.eq(UserEventRole::Participant))
        .filter(
            Condition::any()
                .add(UserEventColumn::ResponseStatus.eq(UserEventResponse::Pending))
                .add(UserEventColumn::ResponseStatus.eq(UserEventResponse::Accepted)),
        )
        .one(&tx)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "You are not a participant in this event.".to_string()))?;

    let mut participant_active = participant.into_active_model();
    participant_active.response_status = Set(UserEventResponse::Declined);
    participant_active
        .update(&tx)
        .await
        .map_err(internal_error)?;

    Busyday::delete_many()
        .filter(BusydayColumn::EventId.eq(id))
        .filter(BusydayColumn::UserId.eq(me))
        .exec(&tx)
        .await
        .map_err(internal_error)?;

    let participant_total = UserEvent::find()
        .filter(UserEventColumn::EventId.eq(id))
        .filter(UserEventColumn::Role.eq(UserEventRole::Participant))
        .count(&tx)
        .await
        .map_err(internal_error)?;

    let was_confirmed = matches!(event.status, EventStatus::Confirmed);
    let mut event_active = event.into_active_model();
    if participant_total <= 1 {
        event_active.status = Set(EventStatus::Canceled);
        Busyday::delete_many()
            .filter(BusydayColumn::EventId.eq(id))
            .exec(&tx)
            .await
            .map_err(internal_error)?;
    } else if was_confirmed {
        event_active.status = Set(EventStatus::Pending);
    }
    event_active.update(&tx).await.map_err(internal_error)?;

    tx.commit().await.map_err(internal_error)?;

    Ok(StatusCode::NO_CONTENT)
}

async fn ensure_event_access(
    db: &DatabaseConnection,
    event_id: Uuid,
    user_id: Uuid,
) -> Result<(), (StatusCode, String)> {
    let event_exists = Event::find_by_id(event_id)
        .one(db)
        .await
        .map_err(internal_error)?
        .is_some();

    if !event_exists {
        return Err((StatusCode::NOT_FOUND, "event not found".to_string()));
    }

    let has_access = UserEvent::find()
        .filter(UserEventColumn::EventId.eq(event_id))
        .filter(UserEventColumn::UserId.eq(user_id))
        .one(db)
        .await
        .map_err(internal_error)?
        .is_some();

    if !has_access {
        return Err((StatusCode::FORBIDDEN, "forbidden".to_string()));
    }

    Ok(())
}

async fn load_event_response(
    db: &DatabaseConnection,
    event_id: Uuid,
) -> Result<EventResponse, (StatusCode, String)> {
    let event = event::Entity::find_by_id(event_id)
        .one(db)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "Event not found.".to_string()))?;

    let participants = load_participants(db, event_id).await?;

    Ok(EventResponse {
        id: event.id,
        creator_id: event.creator_id,
        date: event.date.to_string(),
        title: event.title,
        description: event.description,
        location: event.location,
        status: event.status.to_string(),
        wish_place_id: event.wish_place_id,
        memory_image_base64: event.memory_image_base64,
        created_at: event.created_at.to_rfc3339(),
        participants,
    })
}

async fn load_participants(
    db: &DatabaseConnection,
    event_id: Uuid,
) -> Result<Vec<ParticipantResponse>, (StatusCode, String)> {
    use crate::entities::User;

    let rows = UserEvent::find()
        .filter(UserEventColumn::EventId.eq(event_id))
        .order_by_asc(UserEventColumn::UserId)
        .all(db)
        .await
        .map_err(internal_error)?;

    let mut participants = Vec::new();
    for row in rows {
        if let Ok(Some(user)) = User::find_by_id(row.user_id).one(db).await {
            participants.push(ParticipantResponse {
                user_id: row.user_id,
                username: user.username,
                avatar_url: user.avatar_url,
                bio: user.bio,
                role: row.role.to_string(),
                response_status: row.response_status.to_string(),
            });
        }
    }
    Ok(participants)
}

async fn accepted_event_ids(
    db: &DatabaseConnection,
    user_id: Uuid,
) -> Result<Vec<Uuid>, (StatusCode, String)> {
    let rows = UserEvent::find()
        .filter(UserEventColumn::UserId.eq(user_id))
        .filter(UserEventColumn::ResponseStatus.eq(UserEventResponse::Accepted))
        .all(db)
        .await
        .map_err(internal_error)?;

    Ok(rows.into_iter().map(|row| row.event_id).collect())
}

async fn ensure_day_is_free<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
    date: NaiveDate,
) -> Result<(), (StatusCode, String)> {
    let exists = Busyday::find()
        .filter(BusydayColumn::UserId.eq(user_id))
        .filter(BusydayColumn::Date.eq(date))
        .one(db)
        .await
        .map_err(internal_error)?
        .is_some();

    if exists {
        return Err((
            StatusCode::CONFLICT,
            "This date is already reserved.".to_string(),
        ));
    }

    Ok(())
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

fn parse_date(value: &str) -> Result<NaiveDate, (StatusCode, String)> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid date format. Use YYYY-MM-DD.".to_string()))
}

fn map_db_constraint_error(err: sea_orm::DbErr) -> (StatusCode, String) {
    let message = err.to_string();
    if message.contains("unique") || message.contains("duplicate key") {
        return (StatusCode::CONFLICT, "This date is already reserved.".to_string());
    }
    internal_error(err)
}

fn internal_error<E: std::fmt::Display>(_e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong. Please try again.".to_string())
}
