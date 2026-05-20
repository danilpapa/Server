use crate::api_doc::api_doc::ApiDoc;
use crate::controllers::{
    auth_controller, calendar_controller, event_controller, friendship_controller,
    wish_place_controller,
};
use crate::migration::Migrator;
use axum::Router;
use controllers::users_controller;
use dotenvy::dotenv;
use sea_orm_migration::MigratorTrait;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

mod api_doc;
mod auth;
mod controllers;
mod db;
mod entities;
mod migration;

#[tokio::main]
async fn main() {
    dotenv().ok();

    let db_connection = db::init_db().await.expect("db connection failed");

    Migrator::up(&db_connection, None)
        .await
        .expect("migration failed");

    let app_router = Router::new()
        .merge(SwaggerUi::new("/docs").url("/api-doc/openapi.json", ApiDoc::openapi()))
        .merge(auth_controller::router())
        .merge(users_controller::router())
        .merge(friendship_controller::router())
        .merge(calendar_controller::router())
        .merge(event_controller::router())
        .merge(wish_place_controller::router());

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    println!("Starts on http://{}", addr);
    let listener = TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app_router.with_state(db_connection))
        .await
        .unwrap();
}
