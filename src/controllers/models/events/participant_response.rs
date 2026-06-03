use utoipa::ToSchema;
use uuid::Uuid;

#[derive(serde::Serialize, ToSchema)]
pub struct ParticipantResponse {
    pub user_id: Uuid,
    pub username: String,
    pub avatar_url: Option<String>,
    pub bio: Option<String>,
    pub role: String,
    pub response_status: String,
}
