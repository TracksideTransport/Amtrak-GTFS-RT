//! Process entry for `amtrak-gtfs-rt-service`, the one service in the one container.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    amtrak_gtfs_rt_service::run_service().await
}
