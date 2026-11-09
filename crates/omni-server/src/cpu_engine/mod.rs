pub mod active_guard;
pub mod circuit_breaker;
pub mod generator;
pub mod memory;
pub mod routes;
pub mod slot;
pub mod types;

#[cfg(test)]
pub mod tests;

pub use active_guard::ActiveTaskGuard;
pub use circuit_breaker::{get_default_run_dir, get_os_boot_id, is_pid_alive, CircuitBreakerManager, InflightMarker};
pub use generator::{CpuGenerator, CpuGeneratorConfig, GenerationOutput, PocketCpuGenerator};
pub use memory::{
    compute_cold_load_floor, get_available_physical_memory_bytes, preflight_cold_load,
    preflight_hot_infer, set_mock_available_memory_bytes, HOT_INFER_FLOOR,
};
pub use slot::{is_generation_active, CpuSlot, GLOBAL_GENERATION_ACTIVE};
pub use types::*;

use axum::{
    routing::{get, post},
    Router,
};
use std::sync::Arc;

/// 向应用路由中注册 5 大核心端点
pub fn register_cpu_engine_routes(router: Router, slot: Arc<CpuSlot>) -> Router {
    let cpu_routes = Router::new()
        .route("/v1/health", get(routes::get_health_handler))
        .route(
            "/v1/engines/cpu/session",
            post(routes::post_session_handler).delete(routes::delete_session_handler),
        )
        .route("/v1/tokenize", post(routes::post_tokenize_handler))
        .route("/v1/chat/completions", post(routes::post_chat_completions_handler))
        .with_state(slot);

    router.merge(cpu_routes)
}
